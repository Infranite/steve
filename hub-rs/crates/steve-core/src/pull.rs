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
//!   the repository allows (`gh api repos/{slug}`), leaving out a rebase GitHub says it can't do (the
//!   REST `rebaseable`) and moving on to the next allowed method when GitHub refuses one (#316). The accepted head is the worktree's commit only
//!   when the whole accepted result is committed there (its tree is the worktree's): an edit not
//!   committed goes back to the agent, never merged in part. A result that was invalidated is never
//!   merged, and nothing armed on the host has to be taken back; one invalidated while Steve's merge
//!   call ran is still said, marked and a card for you;
//! - only a change an independent review passed is merged that way: the item's work graph, as accepted,
//!   has its required reviews done. Any other change (no work graph, or a graph accepted as a single
//!   session) waits on a "PR awaiting merge" card that only you answer, never YOLO; your yes lets Steve
//!   merge it as above, and merging it on the host yourself completes the item too;
//! - a merge that fails is tried again later, waiting longer each time ([`MERGE_RETRY_MS`]); after
//!   [`MERGE_FAILS_CARD`] failures in a row, or [`UNKNOWN_LOOKS_CARD`] looks in a row the host could
//!   not answer, a card for you;
//! - merged → the item closes: a pull request this result saw that merged with the accepted head (Steve's
//!   merge, or the host's head was the accepted one) is the evidence itself, whatever became of its
//!   branch (deleted on merge) or the worktree since; otherwise the completion rule
//!   ([`crate::upstream::probe`]) is looked at again and the item closes on what the remote shows. The
//!   pull request is looked up by its number once this result saw it (#347);
//! - a conflict with the default branch, a branch behind it that the host requires up to date, a failed
//!   check, a draft, a head other than the accepted result, or an accepted result not all committed →
//!   the agent is woken with what is wrong, once per pushed head and at most [`PR_TELLS_MAX`] times a
//!   cycle (then a card for you), to fix it on the same branch;
//! - no pull request for the branch, or only the one of an earlier result of the cycle that merged or
//!   closed before this result saw it open → the agent is asked once to push it and open one;
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

use crate::cycle::{
    CLOSING, COMPLETE, Closing, OP_RELEASE, closing_of, conflict, fingerprint_head,
};
use crate::error::Result;
use crate::repo::{Decision, Item, NewDecision};
use crate::thread::flowback::{Update, flow_quiet};
use crate::util::{js_head, now};
use crate::{Core, Tx};

mod closeout;
pub use closeout::{Checks, Closeout, Snapshot};
mod approval;
use approval::MergeTarget;

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
/// The `pr_told` keys of what the agent was told (each followed by what it was about, a head, the
/// checks that failed); every other key is of a card for you.
const TELL_KEYS: [&str; 8] = [
    "no-pr",
    "conflict:",
    "behind:",
    "ci:",
    "draft:",
    "head:",
    "dirty:",
    "before:",
];

/// `key` (of `pr_told`) is of something the agent was told, not of a card for you.
pub(crate) fn is_tell_key(key: &str) -> bool {
    TELL_KEYS.iter().any(|t| key.starts_with(t))
}

/// How long a closing whose worktree moved waits for the host to say whether the pull request it is
/// followed on merged, before the result is void all the same (the host can't be asked).
pub const PR_VERIFY_MS: i64 = 3 * PR_EVERY_MS;

/// The release `o` follows a pull request this result saw that merged with the accepted result
/// (closing `c`): Steve merged it at the accepted head, or the host says its head was the accepted
/// head when it merged (a rebase, a squash or a merge commit of it alike).
pub(crate) fn merged_with(o: &steve_store::ItemOperation, c: &Closing) -> bool {
    let ev = &o.evidence;
    if o.kind != OP_RELEASE
        || ev["pr"].as_u64().is_none()
        || ev["pr_merged"] != json!(true)
        || ev["pr_merged_void"] == json!(true)
    {
        return false;
    }
    let head = c
        .fingerprint
        .as_deref()
        .map(fingerprint_head)
        .filter(|h| !h.is_empty());
    ev["pr_merged_by"] == json!("steve") || head.is_some_and(|h| ev["pr_head"].as_str() == Some(h))
}

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
    /// The check rollup on this head; absent is not an observed empty rollup.
    pub checks: Option<Checks>,
    /// The host is still working out whether it can merge (`UNKNOWN`).
    pub settling: bool,
    /// The commit merging it made on the default branch (merged only): a squash or a rebase copy of
    /// its head, or a merge commit.
    pub merge_commit: Option<String>,
}

impl Pr {
    /// Open, ready for review, every check passed and nothing in the way: it may be merged now.
    pub fn mergeable_now(&self) -> bool {
        self.state == "OPEN"
            && !self.draft
            && !self.head.is_empty()
            && self.checks.is_some()
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
    let checks = p["statusCheckRollup"].as_array().map(|rows| {
        let mut out = Checks::default();
        for c in rows {
            let name = c["name"]
                .as_str()
                .or_else(|| c["context"].as_str())
                .unwrap_or("check");
            let (failed, pending, passed) = if let Some(state) = c["state"].as_str() {
                (
                    matches!(state, "FAILURE" | "ERROR"),
                    matches!(state, "PENDING" | "EXPECTED"),
                    state == "SUCCESS",
                )
            } else if c["status"].as_str().is_some_and(|s| s != "COMPLETED") {
                (false, true, false)
            } else {
                let conclusion = c["conclusion"].as_str();
                (
                    matches!(
                        conclusion,
                        Some(
                            "FAILURE"
                                | "TIMED_OUT"
                                | "CANCELLED"
                                | "ACTION_REQUIRED"
                                | "STARTUP_FAILURE"
                        )
                    ),
                    false,
                    conclusion == Some("SUCCESS"),
                )
            };
            if failed {
                out.failed += 1;
                out.failing.push(name.into());
            } else if pending {
                out.pending += 1;
            } else if passed {
                out.passed += 1;
            } else {
                out.other += 1;
            }
        }
        out.failing.sort();
        out.failing.dedup();
        out
    });
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
        failing: checks
            .as_ref()
            .map(|c| c.failing.clone())
            .unwrap_or_default(),
        pending: checks.as_ref().is_some_and(|c| c.pending > 0),
        checks,
        settling: mergeable == "UNKNOWN" || merge_state == "UNKNOWN",
        merge_commit: p["mergeCommit"]["oid"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(String::from),
    })
}

/// The merge methods the repository allows, from `gh api repos/{slug}` output, in the order Steve
/// asks for them: rebase, squash, merge. The `allow_*` fields are only there for an account that may
/// write to the repository.
pub fn merge_methods(repo: &Value) -> std::result::Result<Vec<&'static str>, String> {
    const METHODS: [(&str, &str); 3] = [
        ("allow_rebase_merge", "--rebase"),
        ("allow_squash_merge", "--squash"),
        ("allow_merge_commit", "--merge"),
    ];
    if METHODS.iter().all(|(k, _)| repo.get(k).is_none()) {
        return Err("读不到仓库允许的合并方式：gh 登录的账号对这个仓库没有写权限".into());
    }
    let allowed: Vec<_> = METHODS
        .into_iter()
        .filter(|(k, _)| repo[k].as_bool() == Some(true))
        .map(|(_, m)| m)
        .collect();
    if allowed.is_empty() {
        return Err("仓库没有允许任何合并方式".into());
    }
    Ok(allowed)
}

/// The first merge method the repository allows ([`merge_methods`]).
pub fn merge_method(repo: &Value) -> std::result::Result<&'static str, String> {
    merge_methods(repo).map(|m| m[0])
}

/// The methods to try for a pull request, of those the repository allows: a rebase is left out when
/// the host already said the branch can't be rebased (`rebaseable == false`, #316); `None` (the host
/// has not worked it out yet) keeps it, and a refusal then moves on to the next one ([`merge_any`]).
pub fn methods_for(allowed: &[&'static str], rebaseable: Option<bool>) -> Vec<&'static str> {
    allowed
        .iter()
        .copied()
        .filter(|m| *m != "--rebase" || rebaseable != Some(false))
        .collect()
}

/// The host refused the merge for its method, not for the pull request (a branch that can't be
/// rebased, a method the repository does not allow): the next allowed method may still merge it.
pub fn method_refused(e: &str) -> bool {
    let e = e.to_lowercase();
    [
        "can't be rebased",
        "cannot be rebased",
        "can not be rebased",
        "merges are not allowed",
        "merge method is not allowed",
        "not allowed on this repository",
    ]
    .iter()
    .any(|w| e.contains(w))
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
pub const PR_FIELDS: &str = "number,state,url,headRefOid,isDraft,mergeable,mergeStateStatus,autoMergeRequest,statusCheckRollup,mergeCommit";

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

/// Pull request `number` of `slug`, by its number (`gh pr view`): what the host says of the one a
/// result is followed on even once its branch was deleted (#347), with the commit merging it made.
pub fn look_number(gh_cli: &Path, slug: &str, number: u64) -> Look {
    match gh(
        gh_cli,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            slug,
            "--json",
            PR_FIELDS,
        ],
    ) {
        Ok(out) => match serde_json::from_str::<Value>(&out) {
            Ok(v) if v.is_object() => read(&json!([v])),
            Ok(_) => Look::Unknown("gh 的输出看不懂".into()),
            Err(e) => Look::Unknown(e.to_string()),
        },
        Err(e) => Look::Unknown(e),
    }
}

/// What a merge Steve asked for came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Merging {
    Merged,
    /// The host could not merge it now (tried again later).
    Failed(String),
    /// No method the repository allows can merge it (each one's reason): yours to look at (#316).
    Refused(String),
}

/// The merge methods `slug` allows ([`merge_methods`]), from the host.
pub fn repo_merge_methods(
    gh_cli: &Path,
    slug: &str,
) -> std::result::Result<Vec<&'static str>, String> {
    let repo = gh(gh_cli, &["api", &format!("repos/{slug}")]).and_then(|out| {
        serde_json::from_str::<Value>(&out).map_err(|e| format!("gh api 的输出看不懂：{e}"))
    })?;
    merge_methods(&repo)
}

/// Whether the host can rebase pull request `number` of `slug` (the REST `rebaseable`); `None` when it
/// has not worked it out yet or can't be asked.
pub fn rebaseable(gh_cli: &Path, slug: &str, number: u64) -> Option<bool> {
    let out = gh(gh_cli, &["api", &format!("repos/{slug}/pulls/{number}")]).ok()?;
    serde_json::from_str::<Value>(&out).ok()?["rebaseable"].as_bool()
}

/// Merges pull request `number` at `head` with the first of `methods` the host takes: a method it
/// refuses for this pull request ([`method_refused`]) moves on to the next one, any other failure
/// stops (tried again later). Every call is pinned to `head` (`--match-head-commit`).
pub fn merge_any(gh_cli: &Path, slug: &str, number: u64, head: &str, methods: &[&str]) -> Merging {
    let mut refused = vec![];
    for m in methods {
        match merge(gh_cli, slug, number, head, m) {
            Merging::Failed(e) if method_refused(&e) => {
                refused.push(format!("{}：{}", method_name(m), js_head(&e, 200)));
            }
            other => return other,
        }
    }
    Merging::Refused(refused.join("；"))
}

/// A merge method in words.
pub fn method_name(m: &str) -> &'static str {
    match m {
        "--rebase" => "rebase 合并",
        "--squash" => "squash 合并",
        _ => "merge commit",
    }
}

/// Merges pull request `number` of `slug` at `head` and nowhere else (`--match-head-commit`: a head
/// pushed since is refused by the host) with `method` ([`repo_merge_methods`]). One `gh` call: it
/// is what runs after the last look at whether the result still stands ([`merge_any`] tries the next
/// method only when the host refused this one, which merged nothing).
pub fn merge(gh_cli: &Path, slug: &str, number: u64, head: &str, method: &str) -> Merging {
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
    steve_runtime::policy::Backoff {
        base: std::time::Duration::from_millis((MERGE_RETRY_MS) as u64),
        max: std::time::Duration::from_millis((MERGE_RETRY_MAX_MS) as u64),
        factor: 2,
        jitter: steve_runtime::policy::Jitter::None,
        attempts: None,
        reset_after: None,
    }
    .delay(doublings, &mut rand::rng())
    .as_millis() as i64
}

/// A followed pull request due for a look.
#[derive(Debug, Clone)]
pub(crate) struct Due {
    op: String,
    item: String,
    slug: String,
    branch: String,
    /// A detached verification can overlap a normal look; don't publish the older result last.
    checked_at: i64,
    /// Looked at only for whether it merged: the branch has nothing of its own (#218).
    only_merged: bool,
    /// The pull request this result is followed on (seen by this result): looked up by its number,
    /// so a deleted branch does not hide it (#347).
    tracked: Option<u64>,
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
    /// On a Node: its last facts saw changes not committed (the same facts as `node_print`).
    node_dirty: bool,
    on_node: bool,
}

/// Where the accepted result is now, for its pull request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Accepted {
    /// At this commit of the worktree: it still holds what was accepted (the same content, or the
    /// same change on a newer default branch).
    At(String),
    /// The worktree still holds what was accepted, but not all of it is committed at this commit (an
    /// edit not committed, a file not added): a pull request at it would not hold the result.
    Uncommitted(String),
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
        let Some(n) = now else {
            return Accepted::Unknown;
        };
        if !crate::cycle::same_result(frozen, &n) {
            return Accepted::Moved;
        }
        /* the fingerprint is of the whole worktree: its commit holds the result only when nothing
         * else is in the worktree (its tree is the commit's) */
        let print = crate::upstream::parse(&n);
        let head = print.head.to_string();
        let committed = if self.on_node {
            Some(!self.node_dirty)
        } else {
            self.cwd.as_deref().and_then(|cwd| {
                crate::git::git(Path::new(cwd), &["rev-parse", &format!("{head}^{{tree}}")])
                    .ok()
                    .map(|t| t.trim() == print.tree)
            })
        };
        match committed {
            Some(true) => Accepted::At(head),
            Some(false) => Accepted::Uncommitted(head),
            None => Accepted::Unknown,
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
            /* the pull request this result is followed on, by its number first: its branch may be
             * gone (merged with "delete branch"); a closed one may have a newer one of its branch */
            let look = match d.tracked.map(|n| look_number(&gh_cli, &d.slug, n)) {
                Some(Look::Found(pr)) if pr.state != "CLOSED" => Look::Found(pr),
                _ => look(&gh_cli, &d.slug, &d.branch),
            };
            let open = matches!(&look, Look::Found(pr) if !d.only_merged && pr.state == "OPEN" && !pr.draft);
            /* looked at only for an open pull request Steve may merge or tell about: the worktree's
             * fingerprint is not free */
            let accepted = if open { d.accepted() } else { Accepted::Moved };
            let merging = match (&look, &accepted) {
                (Look::Found(pr), Accepted::At(h))
                    if !d.only_merged
                        && now() >= d.merge_after
                        && pr.mergeable_now()
                        && &pr.head == h
                        && self.tx().pr_may_merge_at(
                            &d.op,
                            MergeTarget::new(&d.slug, pr.number, h).as_ref(),
                        )? =>
                {
                    /* the methods are read first (and whether the branch can be rebased, #316), so
                     * that between asking again under the lock (a result invalidated since is never
                     * merged) and the merge there are the merge calls alone; the host refuses any
                     * other head */
                    match repo_merge_methods(&gh_cli, &d.slug) {
                        Err(e) => Some(Merging::Failed(e)),
                        Ok(allowed) => {
                            let can_rebase = match allowed.first() {
                                Some(&"--rebase") => rebaseable(&gh_cli, &d.slug, pr.number),
                                _ => None,
                            };
                            let methods = methods_for(&allowed, can_rebase);
                            if methods.is_empty() {
                                Some(Merging::Refused(
                                    "GitHub 说这个分支不能 rebase（分支里有合并提交，或 rebase 会冲突），而仓库只允许 rebase 合并".into(),
                                ))
                            } else if self.tx().pr_may_merge_at(
                                &d.op,
                                MergeTarget::new(&d.slug, pr.number, h).as_ref(),
                            )? {
                                Some(merge_any(&gh_cli, &d.slug, pr.number, h, &methods))
                            } else {
                                None
                            }
                        }
                    }
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
        "Steve 会跟踪分支 {branch} 的 PR 的 CI 和冲突，检查都通过后由 Steve 按验收的版本合并它（没有经过独立评审的改动先等主人批准）：确认 PR 已开好（没有就用 steve_github_open_pr 请求 Steve 推送并开 PR；修 bug 时关联 issue，在 PR 里写 Closes #编号）。不要自己合并、不要推送默认分支、不要手工打 tag（发版按仓库自己的流程）；CI 失败或冲突时 Steve 会叫你在同一个分支上修。"
    )
}

fn merged_update(item: &str, pr: &Pr) -> Update<'static> {
    Update {
        kind: "pr_merged",
        text: format!(
            "{item} 的 PR #{} 已合并（{}）",
            pr.number,
            js_head(&pr.head, 12)
        ),
        ..Default::default()
    }
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
        self.db().store().write(|tx| {
            steve_store::operation_repo::set(
                tx,
                op,
                "pending",
                &json!({"pr_slug": slug, "pr_branch": branch}),
            )
        })?;
        Ok(())
    }

    /// The release of an earlier result of this cycle (not `op`) that found the pull request of
    /// `branch`: the one Steve followed last (`pr_state` what it saw last; `pr_merged` Steve merged it,
    /// or saw it merged).
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

    /// The release of the item's current result, when its pull request merged with the accepted
    /// result ([`merged_with`]): the requirement is met and the worktree no longer matters (#347).
    pub(crate) fn pr_merged_accepted(
        &self,
        i: &Item,
        c: &Closing,
    ) -> Result<Option<steve_store::ItemOperation>> {
        if c.kind != COMPLETE || !c.accepted || c.frozen_revision != i.revision {
            return Ok(None);
        }
        Ok(self
            .db()
            .store()
            .items()
            .operations(&i.id, i.cycle)?
            .into_iter()
            .find(|o| o.revision == i.revision && merged_with(o, c)))
    }

    /// The accepted result's worktree moved while the pull request it is followed on was open (as
    /// this result saw it): `true` while the closing waits for the host to say whether it merged
    /// meanwhile (a merge with "delete branch", a worktree cleaned up after it), which the next look
    /// answers ([`Core::check_prs`], started now). Once the host answered after the move, or could
    /// not be asked for [`PR_VERIFY_MS`], `false`: the moved result is void as before.
    pub(crate) fn pr_merge_unseen(&mut self, i: &Item, c: &Closing) -> Result<bool> {
        if c.kind != COMPLETE || !c.accepted || c.frozen_revision != i.revision {
            return Ok(false);
        }
        let Some(o) = self
            .db()
            .store()
            .items()
            .operations(&i.id, i.cycle)?
            .into_iter()
            .find(|o| {
                o.kind == OP_RELEASE
                    && o.revision == i.revision
                    && o.status == "pending"
                    && o.evidence["pr"].as_u64().is_some()
                    && o.evidence["pr_state"] == json!("OPEN")
                    && o.evidence["pr_merged"] != json!(true)
                    && o.evidence["pr_slug"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
            })
        else {
            return Ok(false);
        };
        let t = now();
        let since = match self.st.pr_verify.get(&o.id) {
            Some(&since) => since,
            None => {
                self.st.pr_verify.insert(o.id.clone(), t);
                /* looked at now, not on the next interval */
                self.st.pr_checked.remove(&o.id);
                let core = self.c.clone();
                crate::detach(move || {
                    if let Err(e) = core.check_prs() {
                        tracing::warn!("pull request check: {e}");
                    }
                });
                tracing::info!(
                    item = %i.id,
                    "closing check: the worktree moved, asking whether PR #{} merged first",
                    o.evidence["pr"]
                );
                return Ok(true);
            }
        };
        let answered = o.evidence["pr_looked"]
            .as_i64()
            .is_some_and(|at| at >= since);
        if answered || t - since >= PR_VERIFY_MS {
            self.st.pr_verify.remove(&o.id);
            return Ok(false);
        }
        Ok(true)
    }

    /// The tracked pull request merged with the accepted result ([`merged_with`]): the requirement of
    /// a project done once merged is met on the host's word, with no look at the worktree (it may be
    /// gone) or the remote branch (deleted on merge). `true` when it was.
    pub(crate) fn pr_merge_meets(
        &mut self,
        i: &Item,
        op: &steve_store::ItemOperation,
    ) -> Result<bool> {
        let Some(c) = closing_of(i) else {
            return Ok(false);
        };
        if !merged_with(op, &c) {
            return Ok(false);
        }
        let n = op.evidence["pr"].as_u64().unwrap_or(0);
        let commit = op.evidence["pr_merge_commit"].as_str().unwrap_or("");
        let what = if commit.is_empty() {
            format!("PR #{n} 已合并")
        } else {
            format!("PR #{n} 已合并（合并提交 {}）", js_head(commit, 7))
        };
        let ev = json!({"rule": crate::upstream::DONE_MERGE, "note": what,
            "landed": (!commit.is_empty()).then_some(commit)});
        self.db()
            .store()
            .write(|tx| steve_store::operation_repo::set(tx, &op.id, "succeeded", &ev))?;
        self.db().timeline_add(
            &i.id,
            "steve",
            "sys",
            &format!(
                "Steve 核对：{what}；按约定（{}）算完成",
                crate::takeover::rule_text(crate::upstream::DONE_MERGE)
            ),
            None,
        )?;
        Ok(true)
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
        } else if let Some(target) = MergeTarget::observed(o)
            && self
                .pr_merge_receipt(i, &target)?
                .is_some_and(|d| d.result["approved"] == true)
        {
            "you"
        } else {
            "none"
        })
    }

    /// Steve may merge the item's pull request right now: its release is still open, the item still
    /// closing on the revision that was accepted, and the change reviewed or yours to have said yes to.
    pub(crate) fn pr_may_merge(&mut self, op: &str) -> Result<bool> {
        let target = self
            .db()
            .store()
            .items()
            .operation(op)?
            .as_ref()
            .and_then(MergeTarget::observed);
        self.pr_may_merge_at(op, target.as_ref())
    }

    /// Recheck consent against the freshly observed target, including immediately before merge.
    fn pr_may_merge_at(&self, op: &str, target: Option<&MergeTarget>) -> Result<bool> {
        let Some(o) = self.db().store().items().operation(op)? else {
            return Ok(false);
        };
        let Some(i) = self.db().item(&o.item_id)? else {
            return Ok(false);
        };
        let accepted = closing_of(&i).is_some_and(|c| c.kind == COMPLETE && c.accepted);
        let receipt = match target {
            Some(target) if o.evidence["pr_slug"].as_str() == Some(&target.repo) => {
                self.pr_merge_receipt(&i, target)?
            }
            _ => None,
        };
        Ok(o.status == "pending"
            && o.kind == OP_RELEASE
            && o.cycle == i.cycle
            && target.is_some()
            && target.is_some_and(|t| o.evidence["pr_slug"].as_str() == Some(t.repo.as_str()))
            && i.phase == CLOSING
            && o.revision == i.revision
            && accepted
            && !receipt
                .as_ref()
                .is_some_and(|d| d.result["approved"] == false)
            && (self.pr_reviewed(&i)? || receipt.is_some_and(|d| d.result["approved"] == true)))
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
            if i.phase != CLOSING || o.cycle != i.cycle || o.revision != i.revision {
                continue;
            }
            self.st.pr_checked.insert(o.id.clone(), t);
            let on_node = self.git_on_node(&i.id);
            out.push(Due {
                op: o.id.clone(),
                item: o.item_id.clone(),
                slug,
                branch,
                checked_at: t,
                only_merged: o.evidence["own"] == json!(false),
                tracked: o.evidence["pr"].as_u64(),
                may_merge: self.pr_may_merge(&o.id)?,
                merge_after: o.evidence["pr_merge_after"].as_i64().unwrap_or(0),
                frozen: closing_of(&i).and_then(|c| c.fingerprint),
                cwd: i.cwd.clone(),
                node_print: on_node
                    .then(|| self.git_seen(&i.id).map(|s| s.print.clone()))
                    .flatten(),
                node_dirty: on_node && self.git_seen(&i.id).is_some_and(|s| s.dirty),
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
        if o.status != "pending"
            || i.phase != CLOSING
            || o.cycle != i.cycle
            || o.revision != i.revision
        {
            /* the result went void while Steve's merge ran (the last look before it was fine): the
             * merge happened all the same, and is never left unsaid */
            if let (Some(Merging::Merged), Look::Found(pr)) = (&merging, &look) {
                self.pr_merged_void(&i, &o, pr)?;
            }
            return Ok(());
        }
        if o.evidence["pr_merged"] == true
            || (o.evidence["pr_observed_since"]
                .as_i64()
                .is_some_and(|at| at > d.checked_at)
                && !matches!(merging, Some(Merging::Merged))
                && !matches!(&look, Look::Found(pr) if pr.state == "MERGED"))
        {
            return Ok(());
        }
        /* a branch with nothing of its own (#218): only a merged pull request counts; anything else
         * (none, open, closed, or the host can't be asked) leaves it to the acceptance alone */
        if o.evidence["own"] == json!(false) {
            let (ev, merged) = match look {
                Look::Found(pr) if pr.state == "MERGED" => (
                    json!({"pr_merged": true, "pr_snapshot": pr.snapshot(), "pr_looked": now()}),
                    Some(pr),
                ),
                _ => (json!({"pr_none": true}), None),
            };
            self.db().store().write::<_, crate::CoreError>(|w| {
                steve_store::operation_repo::set(w, &d.op, "pending", &ev)?;
                if let Some(pr) = merged.filter(|_| o.evidence["pr_merged"] != true) {
                    flow_quiet(self.db(), &i.id, merged_update(&i.id, &pr));
                }
                Ok(())
            })?;
            self.forget_probe(&i.id);
            self.st.pr_checked.remove(&d.op);
            self.lc_try_close(&i.id)?;
            return Ok(());
        }
        let told = strings(&o.evidence["pr_told"]);
        let found = matches!(&look, Look::Found(_));
        let target = match &look {
            Look::Found(pr) => MergeTarget::new(&d.slug, pr.number, &pr.head),
            _ => None,
        };
        let receipt = match &target {
            Some(t) => self.pr_merge_receipt(&i, t)?,
            None => None,
        };
        let declined = receipt
            .as_ref()
            .is_some_and(|d| d.result["approved"] == false);
        let verified = target
            .as_ref()
            .is_some_and(|t| matches!(&accepted, Accepted::At(h) if *h == t.head));
        let review = if self.pr_reviewed(&i)? {
            "graph"
        } else if (verified || matches!(&look, Look::Found(pr) if pr.state == "MERGED"))
            && receipt
                .as_ref()
                .is_some_and(|d| d.result["approved"] == true)
        {
            "you"
        } else {
            "none"
        };
        /* what the agent is told (once per key: a pushed head, a set of failed checks) */
        let mut tell: Option<(String, String)> = None;
        let mut ev = json!({"pr_observed_since": d.checked_at});
        if matches!(look, Look::Found(_)) {
            // These fields describe this observation; only the source decision grants authority.
            ev["pr_merge_ok"] = json!(review == "you");
            ev["pr_merge_declined"] = json!(declined);
            ev["pr_merge_decision"] = json!(receipt.as_ref().map(|d| &d.id));
        }
        let mut close = false;
        let mut signal = None;
        /* a card for you, once per key */
        let mut card: Option<(String, String)> = None;
        let mut ask_merge: Option<(MergeTarget, String)> = None;
        let unknown = o.evidence["pr_unknown"].as_i64().unwrap_or(0);
        if unknown > 0 && !matches!(look, Look::Unknown(_)) {
            ev["pr_unknown"] = json!(0);
        }
        /* when the host last answered: a closing check that saw the worktree move waits for an
         * answer after that before it voids the result (#347) */
        if !matches!(look, Look::Unknown(_)) {
            ev["pr_looked"] = json!(now());
            ev["pr_sync_error"] = Value::Null;
            ev["pr_snapshot"] = Value::Null;
            ev["pr_progress"] = json!("no_pr");
        }
        match look {
            Look::Unknown(e) => {
                let n = unknown + 1;
                ev["pr_unknown"] = json!(n);
                ev["pr_sync_error"] = json!(if e.is_empty() {
                    "gh 未返回错误详情"
                } else {
                    &e
                });
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
                        "（Steve）验收通过了，但分支 {b} 还没有 PR。用 steve_github_open_pr 请求 Steve 推送这个分支并开 PR（修 bug 时关联 issue，在 PR 里写 Closes #编号），然后结束这一回合；检查通过后 Steve 会合并它。不要自己合并，也不要推送默认分支。",
                        b = d.branch
                    ),
                ));
            }
            /* the pull request an earlier result of the cycle followed, found merged or closed
             * before this result ever saw it: it was the earlier result's, and says nothing of this
             * one (nothing is marked merged, no card): the agent hears it needs one of its own */
            Look::Found(pr)
                if pr.state != "OPEN"
                    && o.evidence["pr_before"].as_u64() == Some(pr.number)
                    && o.evidence["pr_state"].is_null() =>
            {
                let how = if pr.state == "MERGED" {
                    "合并"
                } else {
                    "关闭"
                };
                tell = Some((
                    format!("before:{}", pr.number),
                    format!(
                        "（Steve）分支 {b} 的 PR #{n} 在这次验收之前就{how}了（{}），它是之前那个结果的，不算这次验收的结果。这次的改动还不在默认分支上，就用 steve_github_open_pr 请求 Steve 推送分支 {b} 并开新的 PR（或请主人重新打开 PR #{n}），然后结束这一回合；检查通过后 Steve 会合并它。不要自己合并，也不要推送默认分支。",
                        pr.url,
                        b = d.branch,
                        n = pr.number
                    ),
                ));
            }
            Look::Found(pr) => {
                ev["pr_snapshot"] = json!(pr.snapshot());
                ev["pr"] = json!(pr.number);
                ev["pr_url"] = json!(pr.url);
                ev["pr_state"] = json!(pr.state);
                ev["pr_head"] = json!(pr.head);
                if let Some(c) = &pr.merge_commit {
                    ev["pr_merge_commit"] = json!(c);
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
                            self.mark_merged(&i, &pr, review, "elsewhere");
                        }
                        close = true;
                    }
                    "CLOSED" => {
                        ev["pr_progress"] = json!("closed");
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
                        ev["pr_progress"] = json!("conflict");
                        tell = Some((
                            format!("conflict:{}", pr.head),
                            format!(
                                "（Steve）PR #{} 和默认分支有冲突，合并不了：{}。在你的分支上取最新的默认分支（git fetch origin），把它合并进来（git merge origin/默认分支），解决冲突、跑相关检查，然后请主人更新 PR 分支。不要从 shell 推送，包括 rebase 之后的强推。改动内容变了就重新用 steve_deliver 交付。不要合并 PR，也不要推送默认分支。",
                                pr.number, pr.url
                            ),
                        ));
                    }
                    _ if pr.behind => {
                        ev["pr_problem"] = json!("behind");
                        ev["pr_progress"] = json!("behind");
                        tell = Some((
                            format!("behind:{}", pr.head),
                            format!(
                                "（Steve）PR #{} 落后于默认分支，仓库要求合并前先跟上：{}。git fetch origin 后把最新的默认分支合并进来（git merge origin/默认分支），跑相关检查，然后请主人更新 PR 分支。不要从 shell 推送，包括 rebase 之后的强推。不要合并 PR，也不要推送默认分支。",
                                pr.number, pr.url
                            ),
                        ));
                    }
                    _ if !pr.failing.is_empty() => {
                        ev["pr_problem"] = json!("ci");
                        ev["pr_progress"] = json!("checks_failed");
                        ev["pr_failing"] = json!(pr.failing);
                        tell = Some((
                            format!("ci:{}:{}", pr.head, pr.failing.join(",")),
                            format!(
                                "（Steve）PR #{} 的检查没通过：{}（{}）。用 steve_github_ci_log 看失败原因，在同一个分支上修好，跑过相关检查后请主人更新 PR 分支；只是偶发失败就请主人重跑检查。改动内容变了就重新用 steve_deliver 交付。不要合并 PR，也不要推送默认分支。",
                                pr.number,
                                pr.failing.join("、"),
                                pr.url
                            ),
                        ));
                    }
                    _ if pr.draft => {
                        ev["pr_problem"] = json!("draft");
                        ev["pr_progress"] = json!("draft");
                        tell = Some((
                            format!("draft:{}", pr.head),
                            format!(
                                "（Steve）验收通过了，但 PR #{n} 还是草稿，合并不了：{}。请主人把它转为可合并，然后结束这一回合。不要合并 PR，也不要推送默认分支。",
                                pr.url,
                                n = pr.number
                            ),
                        ));
                    }
                    _ => {
                        ev["pr_problem"] = Value::Null;
                        ev["pr_progress"] = json!(if pr.pending {
                            "waiting_checks"
                        } else if pr.settling || pr.checks.is_none() {
                            "waiting_github"
                        } else if !matches!(&accepted, Accepted::At(h) if *h == pr.head) {
                            "waiting_result"
                        } else if o.evidence["pr_merge_after"]
                            .as_i64()
                            .is_some_and(|at| now() < at)
                            && o.evidence["pr_error_head"] == pr.head
                            && o.evidence["pr"] == pr.number
                        {
                            if o.evidence["pr_error_method"] == true {
                                "merge_method"
                            } else {
                                "merge_retry"
                            }
                        } else {
                            "ready"
                        });
                        /* the branch on the host is not what was accepted: the accepted commit was
                         * not pushed, or something else was pushed over it */
                        if let Accepted::At(h) = &accepted
                            && *h != pr.head
                            && !d.on_node
                        {
                            ev["pr_problem"] = json!("head");
                            ev["pr_progress"] = json!("head");
                            tell = Some((
                                format!("head:{}:{h}", pr.head),
                                format!(
                                    "（Steve）PR #{} 的分支头是 {}，不是验收通过的版本 {}，Steve 只合并验收的版本：{}。请主人把验收的提交推送到分支 {}，然后结束这一回合。改动内容变了就重新用 steve_deliver 交付。不要合并 PR，也不要推送默认分支。",
                                    pr.number,
                                    js_head(&pr.head, 12),
                                    js_head(h, 12),
                                    pr.url,
                                    d.branch
                                ),
                            ));
                        }
                        /* the accepted result is not all committed: the pushed head can't hold it */
                        if let Accepted::Uncommitted(h) = &accepted {
                            ev["pr_problem"] = json!("uncommitted");
                            ev["pr_progress"] = json!("uncommitted");
                            tell = Some((
                                format!("dirty:{h}"),
                                format!(
                                    "（Steve）验收通过的结果还有没提交的改动（工作区里有改过没提交的文件，或没加进来的新文件），PR #{} 的分支里没有它们，Steve 不会合并：{}。把改动提交（git add、git commit），请主人推送到分支 {}，然后结束这一回合；不该提交的文件删掉或写进 .gitignore。改动内容变了就重新用 steve_deliver 交付。不要合并 PR，也不要推送默认分支。",
                                    pr.number, pr.url, d.branch
                                ),
                            ));
                        }
                        if review == "none" && !declined && verified {
                            ask_merge = target.clone().map(|t| (t, pr.url.clone()));
                        }
                        /* ready to merge, but nothing says which commit was accepted: Steve does
                         * not merge a head it can't bind to the result */
                        if accepted == Accepted::Unknown
                            && (d.may_merge
                                || review == "graph"
                                || receipt
                                    .as_ref()
                                    .is_some_and(|d| d.result["approved"] == true))
                            && pr.mergeable_now()
                        {
                            ev["pr_problem"] = json!("accepted_unknown");
                            ev["pr_progress"] = json!("accepted_unknown");
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
                    Some(Merging::Refused(why)) => {
                        /* no method the repository allows can merge it: trying again would only be
                         * refused again, so it is yours now, with why (#316); looked at again later */
                        let n = o.evidence["pr_merge_failures"].as_i64().unwrap_or(0) + 1;
                        ev["pr_merge_failures"] = json!(n);
                        ev["pr_merge_after"] = json!(now() + merge_retry_ms(n));
                        ev["pr_merge_error"] = json!(js_head(why, 300));
                        ev["pr_error_head"] = json!(pr.head);
                        ev["pr_error_method"] = json!(true);
                        ev["pr_problem"] = json!("merge_method");
                        ev["pr_progress"] = json!("merge_method");
                        card = Some((
                            format!("method:{}", pr.head),
                            format!(
                                "Steve 合并不了 PR #{}（{}）：仓库允许的合并方式都不行——{}。你可以在 GitHub 上看看，换一种合并方式或自己合并，合并后事项照常完成。",
                                pr.number,
                                pr.url,
                                js_head(why, 300)
                            ),
                        ));
                    }
                    Some(Merging::Failed(e)) => {
                        let n = o.evidence["pr_merge_failures"].as_i64().unwrap_or(0) + 1;
                        let wait = merge_retry_ms(n);
                        ev["pr_merge_failures"] = json!(n);
                        ev["pr_merge_after"] = json!(now() + wait);
                        ev["pr_merge_error"] = json!(js_head(e, 300));
                        ev["pr_error_head"] = json!(pr.head);
                        ev["pr_error_method"] = json!(false);
                        ev["pr_problem"] = json!("merge");
                        ev["pr_progress"] = json!("merge_retry");
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
                if close && o.evidence["pr_merged"] != true {
                    signal = Some(merged_update(&i.id, &pr));
                } else if pr.state == "OPEN" && !pr.failing.is_empty() {
                    let key = json!([pr.number, pr.head, pr.failing]);
                    let mut sent = o.evidence["pr_ci_notified"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    if !sent.contains(&key) {
                        sent.push(key);
                        ev["pr_ci_notified"] = json!(sent);
                        signal = Some(Update {
                            kind: "pr_checks_failed",
                            text: format!(
                                "{} 的 PR #{}（{}）检查失败：{}",
                                i.id,
                                pr.number,
                                js_head(&pr.head, 12),
                                pr.failing.join("、")
                            ),
                            ..Default::default()
                        });
                    }
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
        // Open cards dedupe by their frozen target. A revoked answer may request a fresh card;
        // the old per-result `awaiting` marker must not suppress it forever.
        let decisions = self.db().decisions_of_item(&i.id)?;
        let ask_merge = ask_merge.filter(|(target, _)| {
            !decisions.iter().any(|d| {
                d.status == "open"
                    && d.subject_type == PR_AWAITING_MERGE
                    && d.subject_id == o.id
                    && MergeTarget::requested(&o, &d.id).as_ref() == Some(target)
            })
        });
        if all.len() != told.len() {
            ev["pr_told"] = json!(all);
        }
        // Persist decision creation and the dedupe markers together with the observation. Only
        // the potentially external agent wake and the normal lifecycle gate run after this commit.
        let db = self.db().clone();
        db.store().write::<_, crate::CoreError>(|w| {
            if found {
                self.pr_expire_merge_requests(&i, &o, target.as_ref())?;
            }
            steve_store::operation_repo::set(w, &d.op, "pending", &ev)?;
            if let Some(u) = signal
                && (u.kind == "pr_merged" || card.is_none())
            {
                flow_quiet(&db, &i.id, u);
            }
            if let Some((_, why)) = card {
                self.fail(&i.id, &why)?;
            }
            if let Some((target, url)) = ask_merge {
                self.pr_ask_merge(&i, &d.op, &target, &url)?;
            }
            Ok(())
        })?;
        if let Some(text) = wake {
            self.pr_wake(&i.id, &text)?;
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
        let Some(to) = self.steve_to(item)? else {
            /* its session is in error: nothing would take the message, a card for you instead */
            return self.fail(
                item,
                &format!(
                    "Bot 的会话出错停下了，Steve 没有给它排消息：{}",
                    text.trim_start_matches("（Steve）")
                ),
            );
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
    fn pr_ask_merge(&mut self, i: &Item, op: &str, target: &MergeTarget, url: &str) -> Result<()> {
        let number = target.pr;
        let d = self.db().decision_create(NewDecision {
            workspace_id: i.workspace_id.clone(),
            kind: "approve".into(),
            subject_type: PR_AWAITING_MERGE.into(),
            subject_id: op.into(),
            item_id: Some(i.id.clone()),
            session_id: None,
            question: format!(
                "PR #{number} 待合并：这个事项改了代码，但没有经过独立评审（没有带评审的工作图），Steve 不会自己合并它。看过改动后批准，Steve 会在检查都通过后合并 {} 的 PR #{number}（提交 {}）；同一提交重新交付保留此次批准，你也可以在 GitHub 上自己合并。",
                target.repo, target.head
            ),
            draft: Some(url.into()),
            options: Some(vec!["yes".into(), "no".into()]),
            requested_by: "steve".into(),
        })?;
        let o = self
            .db()
            .store()
            .items()
            .operation(op)?
            .ok_or_else(|| conflict("没有这个收尾操作"))?;
        let mut requests = o.evidence["pr_merge_requests"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        requests.insert(d.id.clone(), json!(target));
        self.db().store().write(|tx| {
            steve_store::operation_repo::set(
                tx,
                op,
                "pending",
                &json!({"pr_merge_requests": requests}),
            )
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
                kind: "pr_approve",
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

    /// Steve merged the pull request of a result that went void during the merge: it is said on the
    /// timeline, marked, kept on the release (a later result does not take it for its own) and a
    /// card for you.
    fn pr_merged_void(&mut self, i: &Item, o: &steve_store::ItemOperation, pr: &Pr) -> Result<()> {
        let ev = json!({"pr": pr.number, "pr_url": pr.url, "pr_state": "MERGED",
            "pr_merged": true, "pr_merged_by": "steve", "pr_merged_void": true});
        self.db()
            .store()
            .write(|tx| steve_store::operation_repo::set(tx, &o.id, &o.status, &ev))?;
        let why = format!(
            "Steve 合并了 PR #{}（{}），但合并时它的结果刚好作废了（事项又有了新的情况）：默认分支上现在有这个改动。看看要不要保留它，还是在 GitHub 上把它撤回。",
            pr.number, pr.url
        );
        self.db().timeline_add(
            &i.id,
            "steve",
            "sys",
            &format!(
                "Steve 已合并 PR #{}（{}），但它的结果在合并时作废了：{}",
                pr.number,
                js_head(&pr.head, 12),
                pr.url
            ),
            None,
        )?;
        let review = self.pr_review(i, o)?;
        self.mark_pr(
            &i.workspace_id,
            &i.id,
            "merged",
            json!({"pr": pr.number, "url": pr.url, "head": pr.head, "review": review,
                "by": "steve", "void": true}),
        );
        self.fail(&i.id, &why)
    }

    /// The `pr` mark of a pull request that merged (`by`: `steve`, or `elsewhere` when it was merged on
    /// the host), with the review the change had, and its `merge` mark for the workflow statistics.
    fn mark_merged(&self, i: &Item, pr: &Pr, review: &str, by: &str) {
        self.mark_merge(&i.workspace_id, &i.id, &pr.url, pr.number); // stats: accepted → merged
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
        if i.phase != CLOSING
            || o.kind != OP_RELEASE
            || o.item_id != i.id
            || o.cycle != i.cycle
            || o.revision != i.revision
            || d.workspace_id != i.workspace_id
        {
            return Err(conflict("这个结果已经作废，不再合并"));
        }
        let target = MergeTarget::requested(&o, &d.id)
            .filter(|t| MergeTarget::observed(&o).as_ref() == Some(t))
            .ok_or_else(|| conflict("批准卡的仓库、PR 或准确提交无法核验，请等待重新核对"))?;
        self.db().decision_resolve(
            &d.id,
            json!({"approved": yes, "text": text, "merge_target": target, "actor": actor}),
            "resolved",
            None,
        )?;
        let pr = o.evidence["pr"].as_u64().unwrap_or(0);
        let ev = json!({"pr_merge_ok": yes, "pr_merge_declined": !yes,
            "pr_merge_by": actor, "pr_merge_decision": d.id});
        self.db()
            .store()
            .write(|tx| steve_store::operation_repo::set(tx, &o.id, "pending", &ev))?;
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
            json!({"statusCheckRollup": null}),
            json!({"state": "MERGED"}),
        ] {
            let Look::Found(p) = read(&json!([pr(extra.clone())])) else {
                panic!()
            };
            assert!(!p.mergeable_now(), "{extra}");
        }
    }

    #[test]
    fn a_rebase_the_host_cannot_do_moves_on_to_the_next_allowed_method() {
        let all = ["--rebase", "--squash", "--merge"];
        assert_eq!(methods_for(&all, Some(false)), ["--squash", "--merge"]);
        assert_eq!(methods_for(&all, None), all);
        assert_eq!(methods_for(&all, Some(true)), all);
        assert!(methods_for(&["--rebase"], Some(false)).is_empty());
        assert!(method_refused(
            "GraphQL: This branch can't be rebased (mergePullRequest)"
        ));
        assert!(method_refused(
            "GraphQL: Squash merges are not allowed on this repository. (mergePullRequest)"
        ));
        assert!(!method_refused(
            "GraphQL: Head branch was modified. Review and try the merge again."
        ));
    }

    #[test]
    fn the_commit_a_merge_made_is_read() {
        let Look::Found(p) = read(&json!([pr(json!({"state": "MERGED",
            "mergeCommit": {"oid": "9c68b6ad6a601c22801d637833b00ca2c0fcc1a9"}}))]))
        else {
            panic!()
        };
        assert_eq!(
            p.merge_commit.as_deref(),
            Some("9c68b6ad6a601c22801d637833b00ca2c0fcc1a9")
        );
        let Look::Found(p) = read(&json!([pr(json!({"mergeCommit": null}))])) else {
            panic!()
        };
        assert_eq!(p.merge_commit, None);
    }

    #[test]
    fn a_failed_merge_waits_longer_each_time_up_to_the_most() {
        assert_eq!(merge_retry_ms(1), MERGE_RETRY_MS);
        assert_eq!(merge_retry_ms(2), 2 * MERGE_RETRY_MS);
        assert_eq!(merge_retry_ms(3), 4 * MERGE_RETRY_MS);
        assert_eq!(merge_retry_ms(100), MERGE_RETRY_MAX_MS);
    }
}

#[cfg(test)]
mod runtime_policy_sequence {
    #[test]
    fn r7_merge_backoff_sequence() {
        for (n, delay) in [
            (0, super::MERGE_RETRY_MS),
            (1, super::MERGE_RETRY_MS),
            (2, 2 * super::MERGE_RETRY_MS),
            (3, 4 * super::MERGE_RETRY_MS),
            (100, super::MERGE_RETRY_MAX_MS),
        ] {
            assert_eq!(super::merge_retry_ms(n), delay);
        }
    }
}
