//! A target whose host takes changes only through a pull request (S6, `host_pr`): the batch is
//! integrated on the Node as for a direct push, then pushed to a branch of its own
//! (`steve/mq/<branch>/MQ-<n>`), one pull request per batch carries it, the host's required checks
//! run on it, and it merges with a merge commit (the API, the batch's commit locked): the members'
//! commits stay ancestors of the target, so their own pull requests show as merged.
//!
//! A merge whose answer is lost is reconciled by reading the pull request: merged is landed, still
//! open is not merged (merged again once), anything else asks. The host's checks past their
//! deadline (from their last change) are no result: a card asks; never retried by itself.
//!
//! The host is behind [`MqHost`] (GitHub's REST API in the Hub); a target whose host has its own
//! merge queue (`host_queue`) is fed to it instead ([`super::drive`]).

use std::time::Duration;

use async_trait::async_trait;

use super::exec::{MqNode, Pushed, Record, Staging, Step};
use super::queue::Outcome;

/// A pull request as the host has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    /// `open` | `closed`
    pub state: String,
    pub merged: bool,
    pub merge_commit: Option<String>,
    pub head_sha: String,
    /// The host's id of it (what a merge queue is fed with).
    pub node_id: Option<String>,
}

/// What the host's checks on a commit say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checks {
    /// None reported: nothing to wait for.
    None,
    /// Some still run; how many have ended (the deadline counts from the last change of it).
    Pending {
        done: usize,
    },
    Passed,
    /// Which failed, and where to read why.
    Failed(String),
}

/// The host of a target's repository (the project's), as the queue uses it.
#[async_trait]
pub trait MqHost: Send + Sync {
    /// The token a push to the project's repository goes with (`None`: none).
    async fn token(&self, project: &str) -> Option<String>;
    /// The open pull request from `head` into `base`, opened now when there is none.
    async fn open_pr(
        &self,
        project: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Step<Pr>;
    async fn pr(&self, project: &str, number: u64) -> Step<Pr>;
    async fn checks(&self, project: &str, sha: &str) -> Step<Checks>;
    /// Merged with a merge commit, only while its head is `sha`: the merge commit.
    async fn merge(&self, project: &str, number: u64, sha: &str, title: &str) -> Step<String>;
    /// Closed (unless merged) and its branch deleted.
    async fn close(&self, project: &str, number: u64, branch: &str);
    /// Handed to the host's merge queue.
    async fn enqueue(&self, project: &str, pr: &Pr) -> Step<()>;
}

/// How a batch lands.
pub enum Land<'a> {
    /// Pushed to the target directly (over the base, a lease).
    Push,
    /// Through a pull request of its own.
    Pr(PrLand<'a>),
}

pub struct PrLand<'a> {
    pub host: &'a dyn MqHost,
    pub project: &'a str,
    /// How often the host's checks are looked at.
    pub poll: Duration,
    /// How long they may stay without a change.
    pub deadline: Duration,
}

/// What landing through a pull request came to.
pub(crate) enum PrOutcome {
    Done(Outcome),
    /// The target moved and the merge was refused: integrated again.
    Moved,
}

/// The batch branch of `batch` (its integration `attempt` after the first gets a suffix: a new
/// branch, never a force push).
pub fn batch_branch(target_branch: &str, batch: u64, attempt: u32) -> String {
    match attempt {
        0 => format!("steve/mq/{target_branch}/MQ-{batch}"),
        n => format!("steve/mq/{target_branch}/MQ-{batch}-{n}"),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn land_pr(
    node: &dyn MqNode,
    s: &Staging,
    l: &PrLand<'_>,
    batch: u64,
    attempt: u32,
    op: &dyn Fn(&str, usize) -> String,
    tip: &str,
    titles: &[String],
    record: &(dyn Fn(Record) + Send + Sync),
) -> PrOutcome {
    let branch = batch_branch(&s.branch, batch, attempt);
    let unknown = |why: String| PrOutcome::Done(Outcome::Unknown { why });
    let o = op("prpush", 0);
    let r = node.push_branch(s, &o, tip, &branch).await;
    record(Record {
        op: o,
        kind: "push",
        state: match &r {
            Pushed::Done => "finished",
            Pushed::Moved => "moved",
            Pushed::Failed(_) => "failed",
            Pushed::Unknown(_) => "unknown",
        },
        detail: Some(branch.clone()),
    });
    match r {
        Pushed::Done => {}
        Pushed::Moved | Pushed::Failed(_) | Pushed::Unknown(_) => {
            let why = match r {
                Pushed::Failed(w) | Pushed::Unknown(w) => w,
                _ => branch.clone(),
            };
            return unknown(steve_i18n::t!("core.mq.push_refused", why = why));
        }
    }
    let title = steve_i18n::t!("core.mq.pr_title", batch = batch, n = titles.len());
    let body = titles
        .iter()
        .map(|t| format!("- {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let pr = match l
        .host
        .open_pr(l.project, &branch, &s.branch, &title, &body)
        .await
    {
        Step::Done(p) => p,
        Step::Failed(w) | Step::Unknown(w) => {
            return unknown(steve_i18n::t!("core.mq.pr_failed", why = w));
        }
    };
    record(Record {
        op: op("pr", 0),
        kind: "pr",
        state: "open",
        detail: Some(pr.number.to_string()),
    });
    // already merged (asked again after a restart)
    if pr.merged {
        return PrOutcome::Done(Outcome::Landed {
            commit: pr.merge_commit.unwrap_or_else(|| tip.to_string()),
        });
    }
    // the host's checks on the batch's commit
    let started = crate::util::now();
    let (mut since, mut seen) = (started, None);
    loop {
        match l.host.checks(l.project, tip).await {
            Step::Done(Checks::None | Checks::Passed) => break,
            Step::Done(Checks::Failed(what)) => {
                l.host.close(l.project, pr.number, &branch).await;
                record(Record {
                    op: op("hostcheck", 0),
                    kind: "check",
                    state: "failed",
                    detail: Some(what.clone()),
                });
                return PrOutcome::Done(Outcome::Failed { tail: what });
            }
            Step::Done(Checks::Pending { done }) => {
                if seen != Some(done) {
                    (since, seen) = (crate::util::now(), Some(done));
                }
                if crate::util::now() - since > l.deadline.as_millis() as i64 {
                    record(Record {
                        op: op("hostcheck", 0),
                        kind: "check",
                        state: "unknown",
                        detail: None,
                    });
                    return unknown(steve_i18n::t!("core.mq.host_checks_late", n = pr.number));
                }
            }
            // the host could not be read now: looked at again, within the same deadline
            Step::Failed(_) | Step::Unknown(_) => {
                if crate::util::now() - started > l.deadline.as_millis() as i64 {
                    return unknown(steve_i18n::t!("core.mq.host_checks_late", n = pr.number));
                }
            }
        }
        tokio::time::sleep(l.poll).await;
    }
    // merged with a merge commit, the batch's commit locked; a lost answer is read off the PR
    for tries in 0..2 {
        let r = l.host.merge(l.project, pr.number, tip, &title).await;
        record(Record {
            op: op("prmerge", tries),
            kind: "pr",
            state: match &r {
                Step::Done(_) => "merged",
                Step::Failed(_) => "refused",
                Step::Unknown(_) => "unknown",
            },
            detail: match &r {
                Step::Done(c) | Step::Failed(c) | Step::Unknown(c) => Some(c.clone()),
            },
        });
        match r {
            Step::Done(commit) => {
                l.host.close(l.project, pr.number, &branch).await;
                return PrOutcome::Done(Outcome::Landed { commit });
            }
            Step::Failed(why) | Step::Unknown(why) => match l.host.pr(l.project, pr.number).await {
                Step::Done(p) if p.merged => {
                    l.host.close(l.project, pr.number, &branch).await;
                    return PrOutcome::Done(Outcome::Landed {
                        commit: p.merge_commit.unwrap_or_else(|| tip.to_string()),
                    });
                }
                // the target moved under it (the host wants it up to date): integrated again
                Step::Done(p) if p.state == "open" && moved_refusal(&why) => {
                    l.host.close(l.project, pr.number, &branch).await;
                    return PrOutcome::Moved;
                }
                // still open: definitely not merged, merged once more
                Step::Done(p) if p.state == "open" && tries == 0 => continue,
                _ => {
                    return unknown(steve_i18n::t!(
                        "core.mq.pr_merge_unknown",
                        n = pr.number,
                        why = why
                    ));
                }
            },
        }
    }
    unknown(steve_i18n::t!(
        "core.mq.pr_merge_unknown",
        n = pr.number,
        why = ""
    ))
}

/// The host refused a merge because the target moved (its rules want the branch up to date).
fn moved_refusal(why: &str) -> bool {
    let w = why.to_lowercase();
    w.contains("base branch was modified")
        || w.contains("head branch was modified")
        || w.contains("not up to date")
        || w.contains("is out-of-date")
}
