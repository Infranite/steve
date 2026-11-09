//! Running one batch on its target's Node (S3): stage at the target's tip, merge the members in
//! order, check what clashes, run the target's own checks, push with a lease, and reconcile a push
//! whose end is not known. Everything goes through the [`MqNode`] port: the Hub decides, the Node
//! does the git (in its own mirror's staging, never the user's repository) and runs the checks in
//! the same sandbox as any run. Each step is a Node operation whose id is derived from the batch,
//! the run and the step, so asking again after a restart takes the step's recorded result instead
//! of doing it again.
//!
//! - a member that conflicts with the base or the members before it is backed out and leaves (no
//!   recorded resolutions: what merges is what was accepted); so does one whose new numbered file
//!   (a migration) takes a number already taken, told the next free one;
//! - the target's checks run only when it has them and the batch is not just the fast-forward of a
//!   head verified on this very base; a check that fails says how (its output's tail), one that
//!   gave no result is unknown, never retried here;
//! - a push the remote refused because the target moved is integrated again on the new tip, at
//!   most `moved_retries` times; a push whose end is unknown is reconciled by reading the target:
//!   at our tip (or containing it) it landed, at the base it did not and is pushed again, moved
//!   elsewhere it is integrated again, unreadable it is unknown. A push is never sent twice under
//!   one operation, and never sent again blind.

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use super::policy::Policy;
use super::queue::Outcome;

/// Where a target is staged: what the port needs to find its Node and staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staging {
    pub target_id: String,
    /// The project whose repository the staging mirrors.
    pub project_id: String,
    pub upstream: String,
    pub branch: String,
    /// Build output kept warm across batches (the toolchains' `mq_keep` directories).
    pub keep: Vec<String>,
}

/// How one step on the Node ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step<T> {
    Done(T),
    /// It did not happen, definitely (git refused, a file missing).
    Failed(String),
    /// Not known whether it happened.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merged {
    /// The staging's new HEAD.
    Done(String),
    /// Backed out: the paths that conflicted.
    Conflict(Vec<String>),
    Failed(String),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    Passed,
    /// A non-zero exit: its output's tail.
    Failed(String),
    /// Killed, past its deadline, the Node gone.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pushed {
    Done,
    /// Refused because the target is not at the base any more: definitely not pushed.
    Moved,
    /// Refused for another reason (credentials, the host's rules).
    Failed(String),
    Unknown(String),
}

/// The Node a target is staged on, as the queue drives it. Operation ids are the caller's: the
/// same id asked again answers what that operation did, never runs it twice.
#[async_trait]
pub trait MqNode: Send + Sync {
    /// The target's upstream fetched where the project is, and the staging reset at the target's
    /// tip (build output in `keep` kept): the base commit.
    async fn stage(&self, s: &Staging, op: &str) -> Step<String>;
    /// `commit` merged into the staging as a merge commit (`message`), a conflict backed out.
    async fn merge(&self, s: &Staging, op: &str, commit: &str, message: &str) -> Merged;
    /// The paths that differ between two commits in the staging.
    async fn changed(&self, s: &Staging, from: &str, to: &str) -> Step<Vec<String>>;
    /// Whether each of `paths` is there at `rev`.
    async fn present(&self, s: &Staging, rev: &str, paths: &[String]) -> Step<Vec<bool>>;
    /// `command` run in the staging, killed at `deadline`.
    async fn check(&self, s: &Staging, op: &str, command: &str, deadline: Duration) -> Checked;
    /// The staging's HEAD (`tip`) pushed to the target's branch, only over `base`.
    async fn push(&self, s: &Staging, op: &str, base: &str, tip: &str) -> Pushed;
    /// Where the target's branch is on its host, read now (`None`: it cannot be told).
    async fn remote(&self, s: &Staging) -> Option<String>;
    /// Whether `ancestor` is in `of`, as last read (`None`: it cannot be told).
    async fn contains(&self, s: &Staging, ancestor: &str, of: &str) -> Option<bool>;
    /// The repository's own release step (`ops/cut-release.py cut`), run in the staging reset at
    /// the target's tip: it picks the version and pushes the branch and its tag itself, waiting
    /// at most `ci_wait` for CI, stopped at `deadline`.
    async fn cut(&self, s: &Staging, op: &str, ci_wait: Duration, deadline: Duration) -> Cut;
    /// The target's tags (name, commit) as its host has them now, fetched where they can be compared
    /// (`None`: it cannot be told).
    async fn tags(&self, s: &Staging) -> Option<Vec<(String, String)>>;
}

/// How a release step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cut {
    /// It ran to its end: its output (`released vX.Y.Z` names the version).
    Done(String),
    /// It refused or failed: its output.
    Failed(String),
    /// Not known whether it released.
    Unknown(String),
}

/// One member of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    pub head: String,
    /// What its merge commit calls it (`ST-12 title`).
    pub label: String,
    /// The base its own checks passed on, when known: a batch of it alone on that base is a
    /// fast-forward of a verified head and runs no checks again.
    pub verified_base: Option<String>,
}

/// A step, as recorded (`mq_steps`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Record {
    pub op: String,
    pub kind: &'static str,
    pub state: &'static str,
    pub detail: Option<String>,
}

/// What running a batch came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// Members that left while integrating, with why (the queue returns them).
    pub left: Vec<(String, String)>,
    /// `None`: every member left (nothing to land).
    pub outcome: Option<Outcome>,
    pub base: Option<String>,
    pub tip: Option<String>,
    /// Each check run: its command and how it ended.
    pub checks: Vec<(String, &'static str)>,
    /// How many times it was integrated again because the target moved.
    pub moved: u32,
}

/// The id of a step of `batch`'s `run` (a run is one start of the batch: a rerun you asked for
/// is a new one), its integration `attempt`, its `kind` and `n`.
pub fn op_id(target: &str, batch: u64, run: u32, attempt: u32, kind: &str, n: usize) -> String {
    steve_node_api::derived_id(
        "op_mq",
        &format!("{target}/{batch}/{run}/{attempt}/{kind}/{n}"),
    )
}

/// A numbered file: its directory and number (`migrations/058.sql` → (`migrations`, 58, 3 digits)).
fn numbered(path: &str) -> Option<(String, u64, usize)> {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    if dir.rsplit('/').next() != Some("migrations") {
        return None;
    }
    let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || name.len() == digits.len() {
        return None;
    }
    Some((dir.to_string(), digits.parse().ok()?, digits.len()))
}

/// The tail of a check's output that a card shows.
pub fn tail(s: &str, lines: usize) -> String {
    let v: Vec<&str> = s.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

/// Runs `batch` (`members` in order) on `node`: integrate, check (`checks` false: you said land it
/// without them), push. `record` gets every step as it goes.
#[allow(clippy::too_many_arguments)]
pub async fn run_batch(
    node: &dyn MqNode,
    s: &Staging,
    batch: u64,
    run: u32,
    members: &[Member],
    policy: &Policy,
    checks: bool,
    record: &(dyn Fn(Record) + Send + Sync),
) -> Ran {
    let mut ran = Ran {
        left: vec![],
        outcome: None,
        base: None,
        tip: None,
        checks: vec![],
        moved: 0,
    };
    let mut attempt: u32 = 0;
    // local steps that came to no known end (a crash, the Node gone mid-merge) are done again
    // from the base, a bounded few times: they changed nothing outside the staging
    let mut local_redo = 0;
    'attempts: loop {
        let op = |kind: &str, n: usize| op_id(&s.target_id, batch, run, attempt, kind, n);
        let base = match node.stage(s, &op("stage", 0)).await {
            Step::Done(b) => b,
            Step::Failed(why) | Step::Unknown(why) => {
                record(Record {
                    op: op("stage", 0),
                    kind: "prepare",
                    state: "failed",
                    detail: Some(why.clone()),
                });
                ran.outcome = Some(Outcome::Unknown {
                    why: steve_i18n::t!("core.mq.stage_failed", why = why),
                });
                return ran;
            }
        };
        record(Record {
            op: op("stage", 0),
            kind: "prepare",
            state: "finished",
            detail: Some(base.clone()),
        });
        ran.base = Some(base.clone());
        let mut tip = base.clone();
        let mut merged: Vec<&Member> = vec![];
        for (i, m) in members.iter().enumerate() {
            if ran.left.iter().any(|(id, _)| *id == m.id) {
                continue;
            }
            // a numbered file whose number is taken already leaves before it merges
            if let Some(why) = clash(node, s, &tip, &m.head).await {
                ran.left.push((m.id.clone(), why));
                continue;
            }
            let msg = format!("Merge {} into {}", m.label, s.branch);
            match node.merge(s, &op("merge", i), &m.head, &msg).await {
                Merged::Done(t) => {
                    record(Record {
                        op: op("merge", i),
                        kind: "merge",
                        state: "finished",
                        detail: Some(t.clone()),
                    });
                    tip = t;
                    merged.push(m);
                }
                Merged::Conflict(paths) => {
                    record(Record {
                        op: op("merge", i),
                        kind: "merge",
                        state: "conflict",
                        detail: Some(paths.join("\n")),
                    });
                    ran.left.push((
                        m.id.clone(),
                        steve_i18n::t!(
                            "core.mq.conflict",
                            files = paths.join(", "),
                            base = short(&base)
                        ),
                    ));
                }
                Merged::Failed(why) => {
                    ran.left.push((
                        m.id.clone(),
                        steve_i18n::t!("core.mq.merge_failed", why = why),
                    ));
                }
                Merged::Unknown(why) => {
                    record(Record {
                        op: op("merge", i),
                        kind: "merge",
                        state: "unknown",
                        detail: Some(why.clone()),
                    });
                    if local_redo < 2 {
                        local_redo += 1;
                        attempt += 1;
                        continue 'attempts;
                    }
                    ran.outcome = Some(Outcome::Unknown { why });
                    return ran;
                }
            }
        }
        ran.tip = Some(tip.clone());
        if merged.is_empty() {
            return ran;
        }
        // the target's own checks, unless this is a verified head fast-forwarded on its own base
        let verified =
            merged.len() == 1 && merged[0].verified_base.as_deref() == Some(base.as_str());
        if checks && !verified && !policy.checks.is_empty() {
            let deadline = Duration::from_secs(policy.check_deadline_s.max(1));
            for (n, cmd) in policy.checks.iter().enumerate() {
                let o = op("check", n);
                let r = node.check(s, &o, cmd, deadline).await;
                let (state, detail) = match &r {
                    Checked::Passed => ("passed", None),
                    Checked::Failed(t) => ("failed", Some(t.clone())),
                    Checked::Unknown(w) => ("unknown", Some(w.clone())),
                };
                record(Record {
                    op: o,
                    kind: "check",
                    state,
                    detail,
                });
                ran.checks.push((cmd.clone(), state));
                match r {
                    Checked::Passed => {}
                    Checked::Failed(t) => {
                        ran.outcome = Some(Outcome::Failed {
                            tail: format!("$ {cmd}\n{}", tail(&t, 40)),
                        });
                        return ran;
                    }
                    Checked::Unknown(why) => {
                        ran.outcome = Some(Outcome::Unknown {
                            why: steve_i18n::t!("core.mq.check_unknown", cmd = cmd, why = why),
                        });
                        return ran;
                    }
                }
            }
        }
        // the push, and what reconciling an unknown one says
        let mut pushes = 0;
        loop {
            let o = op("push", pushes);
            let r = node.push(s, &o, &base, &tip).await;
            let state = match &r {
                Pushed::Done => "finished",
                Pushed::Moved => "moved",
                Pushed::Failed(_) => "failed",
                Pushed::Unknown(_) => "unknown",
            };
            record(Record {
                op: o,
                kind: "push",
                state,
                detail: match &r {
                    Pushed::Failed(w) | Pushed::Unknown(w) => Some(w.clone()),
                    _ => None,
                },
            });
            let moved = match r {
                Pushed::Done => {
                    ran.outcome = Some(Outcome::Landed { commit: tip });
                    return ran;
                }
                Pushed::Moved => true,
                Pushed::Failed(why) => {
                    ran.outcome = Some(Outcome::Unknown {
                        why: steve_i18n::t!("core.mq.push_refused", why = why),
                    });
                    return ran;
                }
                Pushed::Unknown(why) => match reconcile(node, s, &base, &tip).await {
                    Reconciled::Landed => {
                        record(Record {
                            op: op("reconcile", pushes),
                            kind: "reconcile",
                            state: "landed",
                            detail: None,
                        });
                        ran.outcome = Some(Outcome::Landed { commit: tip });
                        return ran;
                    }
                    Reconciled::NotPushed if pushes < 1 => {
                        record(Record {
                            op: op("reconcile", pushes),
                            kind: "reconcile",
                            state: "not_pushed",
                            detail: None,
                        });
                        pushes += 1;
                        continue;
                    }
                    Reconciled::Moved => true,
                    Reconciled::NotPushed | Reconciled::Unknown => {
                        record(Record {
                            op: op("reconcile", pushes),
                            kind: "reconcile",
                            state: "unknown",
                            detail: Some(why.clone()),
                        });
                        ran.outcome = Some(Outcome::Unknown {
                            why: steve_i18n::t!("core.mq.push_unknown", why = why),
                        });
                        return ran;
                    }
                },
            };
            if moved {
                if attempt >= policy.moved_retries + local_redo {
                    ran.outcome = Some(Outcome::Unknown {
                        why: steve_i18n::t!("core.mq.target_moving", n = ran.moved),
                    });
                    return ran;
                }
                ran.moved += 1;
                attempt += 1;
                continue 'attempts;
            }
        }
    }
}

enum Reconciled {
    Landed,
    /// The target is still at the base: definitely not pushed.
    NotPushed,
    /// The target moved elsewhere without our tip.
    Moved,
    Unknown,
}

async fn reconcile(node: &dyn MqNode, s: &Staging, base: &str, tip: &str) -> Reconciled {
    match node.remote(s).await {
        None => Reconciled::Unknown,
        Some(r) if r == tip => Reconciled::Landed,
        Some(r) if r == base => Reconciled::NotPushed,
        Some(r) => match node.contains(s, tip, &r).await {
            Some(true) => Reconciled::Landed,
            Some(false) => Reconciled::Moved,
            None => Reconciled::Unknown,
        },
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Why `head` may not merge onto `tip`: a numbered file (a migration) it adds whose number a
/// different file of `tip` has already, with the next free number. `None`: no clash, or it cannot
/// be told (the merge then decides).
async fn clash(node: &dyn MqNode, s: &Staging, tip: &str, head: &str) -> Option<String> {
    let Step::Done(paths) = node.changed(s, tip, head).await else {
        return None;
    };
    let nums: Vec<(String, (String, u64, usize))> = paths
        .iter()
        .filter_map(|p| numbered(p).map(|n| (p.clone(), n)))
        .collect();
    if nums.is_empty() {
        return None;
    }
    let names: Vec<String> = nums.iter().map(|(p, _)| p.clone()).collect();
    let (Step::Done(in_tip), Step::Done(in_head)) = (
        node.present(s, tip, &names).await,
        node.present(s, head, &names).await,
    ) else {
        return None;
    };
    for (i, (p, (dir, n, _))) in nums.iter().enumerate() {
        // what it adds: there in its head, not on the tip
        if !(in_head[i] && !in_tip[i]) {
            continue;
        }
        let taken = nums
            .iter()
            .enumerate()
            .any(|(j, (q, (d, m, _)))| j != i && d == dir && m == n && q != p && in_tip[j]);
        if taken {
            let width = nums[i].1.2;
            let next = nums
                .iter()
                .enumerate()
                .filter(|(j, (_, (d, _, _)))| d == dir && (in_tip[*j] || in_head[*j]))
                .map(|(_, (_, (_, m, _)))| *m)
                .max()
                .unwrap_or(*n)
                + 1;
            return Some(steve_i18n::t!(
                "core.mq.numbered_taken",
                path = p,
                n = format!("{n:0width$}"),
                next = format!("{next:0width$}")
            ));
        }
    }
    None
}

/// The version a release of `heads` went out as: the oldest `vX.Y.Z` of `tags` whose commit
/// contains every one of them (asked of the Node). `None` when no single tag does.
async fn released_as(
    node: &dyn MqNode,
    s: &Staging,
    heads: &[String],
    tags: &[(String, String)],
) -> Option<String> {
    let mut versions: Vec<(Vec<u64>, String, String)> = tags
        .iter()
        .filter_map(|(name, sha)| {
            let v = name.strip_prefix('v')?;
            let parts: Option<Vec<u64>> = v.split('.').map(|p| p.parse().ok()).collect();
            parts
                .filter(|p| p.len() == 3)
                .map(|p| (p, v.to_string(), sha.clone()))
        })
        .collect();
    versions.sort();
    for (_, v, sha) in versions {
        let mut all = true;
        for h in heads {
            if node.contains(s, h, &sha).await != Some(true) {
                all = false;
                break;
            }
        }
        if all {
            return Some(v);
        }
    }
    None
}

/// The highest `vX.Y.Z` of `tags`.
fn highest(tags: &[(String, String)]) -> Option<Vec<u64>> {
    tags.iter()
        .filter_map(|(name, _)| {
            let parts: Option<Vec<u64>> = name
                .strip_prefix('v')?
                .split('.')
                .map(|p| p.parse().ok())
                .collect();
            parts.filter(|p| p.len() == 3)
        })
        .max()
}

/// The release script's output for a card: what refused first (`✗ …`, with what to do before
/// retrying when it is about CI), then the rest, cut.
pub fn refusal(output: &str) -> String {
    let (why, rest): (Vec<&str>, Vec<&str>) = output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .partition(|l| l.trim_start().starts_with('✗'));
    let what = why.join("\n");
    let lead = if what.contains("gh run rerun") {
        steve_i18n::t!("core.mq.cut_ci_failed")
    } else if what.contains("CI") && what.contains("is still") {
        steve_i18n::t!("core.mq.cut_ci_running")
    } else {
        String::new()
    };
    let all = format!("{lead}{}", [what, rest.join("\n")].join("\n"));
    crate::util::js_head(all.trim(), 600).to_string()
}

/// Releases train `train` (`heads`: its changes' commits) on `node`, once: a release of these
/// commits that went out already (a restart between the release and its record) is found first and
/// not cut again; a cut whose end is unknown is reconciled by the target's tags: one containing
/// every change is that release, the highest tag unchanged is no release, anything else is unknown.
#[allow(clippy::too_many_arguments)]
pub async fn run_cut(
    node: &dyn MqNode,
    s: &Staging,
    train: u64,
    run: u32,
    heads: &[String],
    ci_wait: Duration,
    deadline: Duration,
    record: &(dyn Fn(Record) + Send + Sync),
) -> super::train::CutResult {
    use super::train::CutResult;
    let op = steve_node_api::derived_id("op_mqcut", &format!("{}/{train}/{run}", s.target_id));
    let Some(before) = node.tags(s).await else {
        return CutResult::Unknown {
            why: steve_i18n::t!("core.mq.tags_unread"),
        };
    };
    if let Some(v) = released_as(node, s, heads, &before).await {
        record(Record {
            op: op.clone(),
            kind: "cut",
            state: "released_before",
            detail: Some(v.clone()),
        });
        return CutResult::Released { version: Some(v) };
    }
    let r = node.cut(s, &op, ci_wait, deadline).await;
    let (state, detail) = match &r {
        Cut::Done(o) => ("finished", o.clone()),
        Cut::Failed(o) => ("failed", o.clone()),
        Cut::Unknown(w) => ("unknown", w.clone()),
    };
    record(Record {
        op: op.clone(),
        kind: "cut",
        state,
        detail: Some(tail(&detail, 40)),
    });
    match r {
        Cut::Done(out) => CutResult::Released {
            version: out
                .lines()
                .rev()
                .find_map(|l| l.trim().strip_prefix("released v").map(str::to_string)),
        },
        Cut::Failed(out) => CutResult::Failed { why: refusal(&out) },
        Cut::Unknown(why) => match node.tags(s).await {
            Some(after) => match released_as(node, s, heads, &after).await {
                Some(v) => CutResult::Released { version: Some(v) },
                None if highest(&after) == highest(&before) => CutResult::Failed {
                    why: steve_i18n::t!("core.mq.cut_not_released", why = why),
                },
                None => CutResult::Unknown { why },
            },
            None => CutResult::Unknown { why },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_files_are_migrations_with_a_leading_number() {
        assert_eq!(
            numbered("hub-rs/crates/steve-store/migrations/058.sql"),
            Some(("hub-rs/crates/steve-store/migrations".into(), 58, 3))
        );
        assert_eq!(
            numbered("migrations/10_add.sql"),
            Some(("migrations".into(), 10, 2))
        );
        assert_eq!(numbered("migrations/LOCK"), None);
        assert_eq!(numbered("src/058.sql"), None);
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
    }
}
