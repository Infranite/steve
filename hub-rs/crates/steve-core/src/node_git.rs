//! The git of an item whose work is in a workspace on a Node (N0e-E11b): what the closing gate, the
//! release read about it is asked of that Node off the core's lock, and a
//! rebase before a release is applied there only on the facts it was decided on.
//!
//! The core never waits on a Node inside its critical section. A section that needs an item's git
//! takes the facts gathered for it ([`Tx::git_take`]); when there are none fresh it starts a
//! gathering and goes on as if it can't tell yet: the closing holds (an unreachable Node is
//! unknown, never "changed"), nothing is released. The facts come back through
//! [`Core::git_arrived`], which checks the closing again. A gathering asked for while one runs is
//! run again after it, so facts that come back are never older than the last ask; the periodic
//! closing check asking for the same facts again is no such ask.
//!
//! Without the port ([`Core::set_node_git`]), or for an item the port says is not on a Node, every
//! read is the Hub's own git in `items.cwd`, as before.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use steve_node_api as na;

use crate::cycle::{CLOSING, CloseKind, same_result};
use crate::orchestrator::To;
use crate::repo::Item;
use crate::util::now;
use crate::{Core, Tx, detach};

/// How long one gathering (or a rebase) may take before it counts as unknown.
const GATHER_DEADLINE: Duration = Duration::from_secs(180);
/// A gathering that has not come back after this is taken for lost.
const GATHER_STUCK_MS: i64 = 5 * 60_000;

/// A query without its workspace: the port knows which one is the item's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ask {
    pub refs: Vec<String>,
    pub ranges: Vec<(String, String)>,
    pub fetch: bool,
    pub reads: Vec<na::GitRead>,
    /// HEAD and the branch only: no status scan (see [`na::GitQuery::heads_only`]).
    pub heads_only: bool,
}

/// How a git action on a Node ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    /// Done: the facts after it.
    Done(na::GitFacts),
    /// Refused or failed, the workspace as before (`stale`: it moved off the facts; `conflict`: a
    /// conflict backed out, the paths one per line; `busy`: uncommitted changes).
    Failed(na::Error),
    /// It may or may not have happened: never retried, said so.
    Unknown(String),
}

/// The Hub's way to the Node an item's git is on.
#[async_trait]
pub trait NodeGit: Send + Sync {
    /// Whether the item's git is in a workspace on a Node (its latest bound session's). A store
    /// read, called under the core's lock: it never waits on a Node.
    fn on_node(&self, item: &str) -> bool;
    /// The facts `q` asks for in the item's workspace; `offline` when its Node is not there.
    async fn facts(&self, item: &str, q: Ask) -> na::ApiResult<na::GitFacts>;
    /// Applies `action` in the item's workspace on the facts `on`, through the Hub's ledger as
    /// `operation` for `subject`, and waits for its end.
    async fn apply(
        &self,
        item: &str,
        operation: &str,
        subject: &str,
        on: &na::GitFacts,
        action: na::GitAction,
    ) -> Applied;
}

/// What the core asks about an item's worktree in one gathering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Want {
    /// The frozen fingerprint a change is described against.
    pub frozen: Option<String>,
    /// The item's branch, for whether there is anything to release.
    pub branch: Option<String>,
}

/// What a gathering found in a git workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    pub head: String,
    /// `head:tree[:base:patch]`, as [`crate::cycle::fingerprint`] writes it.
    pub print: String,
    /// What it was asked for.
    pub want: Want,
    /// The change from `want.frozen` in words, when the result moved.
    pub moved: Option<String>,
    /// Anything a release could ship ([`crate::git::has_changes`]); `None`: can't tell.
    pub changes: Option<bool>,
    /// The worktree has changes not committed (tracked or untracked), as `print` was taken.
    pub dirty: bool,
}

/// A gathering's result: `Ok(None)` is no git checkout there; `Err` is can't tell now (the Node
/// is offline, git did not answer).
pub(crate) type Gathered = std::result::Result<Option<Seen>, String>;

/// The git facts of one item.
#[derive(Debug, Default)]
pub(crate) struct Slot {
    last: Option<Gathered>,
    /// Not yet decided on.
    fresh: bool,
    /// The critical section that took them (it may read them again).
    taken_in: u64,
    /// Asks so far, and the running gathering: the ask it serves, since when, what it asks.
    asked: u64,
    running: Option<(u64, i64, Want)>,
}

/// Runs `f` to completion from synchronous code that holds no lock: on the caller's runtime when
/// there is one (a blocking thread of it), else on a small one of its own.
pub(crate) fn block_on<F: std::future::Future>(f: F) -> Option<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => Some(match h.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| h.block_on(f))
            }
            _ => h.block_on(f),
        }),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
            .map(|rt| rt.block_on(f)),
    }
}

fn first_answer(f: &na::GitFacts) -> Option<&na::GitAnswer> {
    f.reads.first().map(|r| &r.answer)
}

/// Gathers `want` for `item` through `port`.
pub(crate) async fn gather(port: &dyn NodeGit, item: &str, want: Want) -> Gathered {
    let mut refs = vec!["main".to_string()];
    if let Some(b) = &want.branch {
        refs.push(format!("refs/heads/{b}"));
    }
    let f = match port
        .facts(
            item,
            Ask {
                refs,
                reads: vec![na::GitRead::Fingerprint],
                ..Default::default()
            },
        )
        .await
    {
        Ok(f) => f,
        // a workspace that is no git checkout with a commit
        Err(e) if e.code == na::Code::Invalid => return Ok(None),
        Err(e) => return Err(e.safe_detail),
    };
    let Some(na::GitAnswer::Fingerprint { tree, own }) = first_answer(&f) else {
        return Err("the Node gave no fingerprint".into());
    };
    let print = match own {
        Some(o) => format!("{}:{tree}:{}:{}", f.head, o.base, o.patch),
        None => format!("{}:{tree}", f.head),
    };
    let changes = gathered_changes(port, item, &want, &f).await;
    let moved = gathered_movement(port, item, &want, &print).await;
    Ok(Some(Seen {
        head: f.head.clone(),
        print,
        want,
        moved,
        changes,
        dirty: f.dirty,
    }))
}

async fn gathered_movement(
    port: &dyn NodeGit,
    item: &str,
    want: &Want,
    print: &str,
) -> Option<String> {
    let moved = match &want.frozen {
        Some(frozen) if !same_result(frozen, &print) => {
            let pairs = crate::upstream::describe_pairs(frozen, &print);
            let reads = pairs
                .iter()
                .map(|(from, to)| na::GitRead::ChangedPaths {
                    from: from.clone(),
                    to: to.clone(),
                    max: 1000,
                })
                .collect();
            let names: Vec<Vec<String>> = match port
                .facts(
                    item,
                    Ask {
                        reads,
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(r) => r
                    .reads
                    .iter()
                    .map(|r| match &r.answer {
                        na::GitAnswer::ChangedPaths { paths, .. } => paths.clone(),
                        _ => vec![],
                    })
                    .collect(),
                Err(_) => vec![],
            };
            let of = |from: &str, to: &str| {
                pairs
                    .iter()
                    .position(|(a, b)| a == from && b == to)
                    .and_then(|i| names.get(i).cloned())
                    .unwrap_or_default()
            };
            Some(crate::upstream::describe_with(frozen, &print, of))
        }
        _ => None,
    };
    moved
}

async fn gathered_changes(
    port: &dyn NodeGit,
    item: &str,
    want: &Want,
    f: &na::GitFacts,
) -> Option<bool> {
    let ref_of = |r: &str| {
        f.refs
            .iter()
            .find(|(n, _)| n == r)
            .and_then(|(_, sha)| sha.clone())
    };
    /* anything to release: the branch beyond main, and when the workspace is on the item's branch
     * (its worktree) its own commits and changes too */
    let changes = match (&want.branch, ref_of("main")) {
        (None, _) | (_, None) => None,
        (Some(b), Some(_)) => {
            let branch = ref_of(&format!("refs/heads/{b}"));
            let on_branch = f.branch.as_deref() == Some(b.as_str());
            let mut ranges = vec![];
            if branch.is_some() {
                ranges.push(("main".to_string(), format!("refs/heads/{b}")));
            }
            if on_branch {
                ranges.push(("main".to_string(), "HEAD".to_string()));
            }
            if ranges.is_empty() {
                Some(false)
            } else {
                match port
                    .facts(
                        item,
                        Ask {
                            ranges,
                            ..Default::default()
                        },
                    )
                    .await
                {
                    Ok(r) => Some(r.ranges.iter().any(|r| r.ahead > 0) || (on_branch && r.dirty)),
                    Err(_) => None,
                }
            }
        }
    };
    changes
}

/// How a rebase before a release went, in `git::Rebase`'s terms.
async fn rebase(port: &dyn NodeGit, item: &str, operation: &str) -> crate::git::Rebase {
    use crate::git::Rebase;
    let f = match port
        .facts(
            item,
            Ask {
                reads: vec![
                    na::GitRead::Status,
                    na::GitRead::IsAncestor {
                        ancestor: "main".into(),
                        of: "HEAD".into(),
                    },
                ],
                ..Default::default()
            },
        )
        .await
    {
        Ok(f) => f,
        Err(e) => return Rebase::Error(e.safe_detail),
    };
    let mut tracked = vec![];
    let mut on_main = false;
    for r in &f.reads {
        match &r.answer {
            na::GitAnswer::Status { entries } => tracked.extend(
                entries
                    .iter()
                    .filter(|e| e.code != "??")
                    .map(|e| format!("{} {}", e.code, e.path.as_str())),
            ),
            na::GitAnswer::IsAncestor { yes } => on_main = *yes,
            _ => {}
        }
    }
    if !tracked.is_empty() {
        tracked.truncate(10);
        return Rebase::Dirty(tracked.join("\n"));
    }
    if on_main {
        return Rebase::Ok { moved: false };
    }
    let action = na::GitAction::Rebase {
        onto: "main".into(),
    };
    let subject = format!("release:{item}");
    match port.apply(item, operation, &subject, &f, action).await {
        Applied::Done(after) => Rebase::Ok {
            moved: after.head != f.head,
        },
        Applied::Failed(e) => match e.code {
            na::Code::Conflict => Rebase::Conflict(
                e.safe_detail
                    .lines()
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join("、"),
            ),
            na::Code::Busy => Rebase::Dirty(e.safe_detail),
            na::Code::Stale => Rebase::Error("核对之后分支又动了".into()),
            na::Code::Unavailable => Rebase::Error("设备离线".into()),
            _ => Rebase::Error(e.safe_detail),
        },
        Applied::Unknown(why) => Rebase::Error(format!("结果未知（{why}），不会自动重试")),
    }
}

impl Core {
    /// Wires the Hub's way to items' Nodes in (once).
    pub fn set_node_git(&self, g: Arc<dyn NodeGit>) {
        let _ = self.0.node_git.set(g);
    }

    pub(crate) fn node_git(&self) -> Option<Arc<dyn NodeGit>> {
        self.0.node_git.get().cloned()
    }

    /// Gathers an item's facts now, before a critical section that decides on them (a delivery
    /// freezing its result, a publish, an answered card). Never under the lock: the caller holds
    /// none. Nothing for an item that is not on a Node.
    pub(crate) fn git_before(&self, item: &str) {
        let Some(port) = self.node_git() else {
            return;
        };
        if !port.on_node(item) {
            return;
        }
        let (ask, want) = {
            let mut t = self.tx();
            let Ok(Some(i)) = t.db().item(item) else {
                return;
            };
            let want = t.git_want(&i);
            let s = t.st.git.entry(item.into()).or_default();
            s.asked += 1;
            s.running = Some((s.asked, now(), want.clone()));
            (s.asked, want)
        };
        let Some(got) = block_on(async {
            tokio::time::timeout(GATHER_DEADLINE, gather(&*port, item, want))
                .await
                .unwrap_or_else(|_| Err("timed out".into()))
        }) else {
            let mut t = self.tx();
            if let Some(s) = t.st.git.get_mut(item)
                && s.running.as_ref().is_some_and(|(a, _, _)| *a == ask)
            {
                s.running = None;
            }
            return;
        };
        let mut t = self.tx();
        let s = t.st.git.entry(item.into()).or_default();
        if s.running.as_ref().is_some_and(|(a, _, _)| *a == ask) {
            s.running = None;
        }
        /* only the newest ask makes them fresh; a gathering of an older one still running stays */
        if s.asked == ask {
            s.last = Some(got);
            s.fresh = true;
        }
    }

    /// A gathering came back: kept, and the closing it was for is checked again (with the
    /// release reports that waited for it). One asked for meanwhile runs now.
    pub(crate) fn git_arrived(&self, item: &str, ask: u64, got: Gathered) {
        let mut t = self.tx();
        let s = t.st.git.entry(item.into()).or_default();
        if s.running.as_ref().is_some_and(|(a, _, _)| *a != ask) {
            return;
        }
        s.running = None;
        if s.asked > ask {
            /* asked again while it ran: what it found may be older than that ask */
            s.last = Some(got);
            let Ok(Some(i)) = t.db().item(item) else {
                return;
            };
            t.git_gather(&i);
            return;
        }
        s.last = Some(got);
        s.fresh = true;
        match t.db().item(item) {
            Ok(Some(i)) if i.phase == CLOSING => {
                if let Err(e) = t.evaluate_cycle_close(item) {
                    tracing::warn!(item, "closing check: {e}");
                }
            }
            _ => {}
        }
    }
}

impl Tx<'_> {
    /// Whether the item's git is read on its Node.
    pub(crate) fn git_on_node(&self, item: &str) -> bool {
        self.c.node_git().is_some_and(|g| g.on_node(item))
    }

    /// What a gathering for `i` asks now: the frozen fingerprint, its branch, and whether each
    /// release of this cycle (and each report waiting) contains the frozen `HEAD`.
    pub(crate) fn git_want(&self, i: &Item) -> Want {
        let frozen = i
            .closing
            .clone()
            .filter(|c| i.phase == CLOSING && c.kind == CloseKind::Complete)
            .and_then(|c| c.fingerprint);
        Want {
            frozen,
            branch: i.branch.clone().filter(|b| !b.is_empty()),
        }
    }

    /// Starts a gathering for `i` (one at a time; one asked for while it runs runs after it).
    pub(crate) fn git_gather(&mut self, i: &Item) {
        let Some(port) = self.c.node_git() else {
            return;
        };
        let want = self.git_want(i);
        let t = now();
        let polling = self.st.git_polling;
        let s = self.st.git.entry(i.id.clone()).or_default();
        if let Some((_, since, serves)) = &s.running
            && t - since < GATHER_STUCK_MS
        {
            /* the periodic check asking for the same facts again is no reason to outdate them
             * (else a gathering slower than its period never comes back fresh); anything else
             * runs again after it */
            if !(polling && *serves == want) {
                s.asked += 1;
            }
            return;
        }
        s.asked += 1;
        let ask = s.asked;
        s.running = Some((ask, t, want.clone()));
        let (core, item) = (self.c.clone(), i.id.clone());
        detach(move || {
            let got = block_on(async {
                tokio::time::timeout(GATHER_DEADLINE, gather(&*port, &item, want))
                    .await
                    .unwrap_or_else(|_| Err("timed out".into()))
            })
            .unwrap_or_else(|| Err("no runtime".into()));
            core.git_arrived(&item, ask, got);
        });
    }

    /// The facts gathered for `i` to decide on in this critical section: fresh ones (taken now,
    /// and again by the same section), else a gathering starts and `None`. Facts gathered for
    /// another closing than the one there is now are not fresh.
    pub(crate) fn git_take(&mut self, i: &Item) -> Option<Gathered> {
        let seq = self.st.tx_seq;
        let want = self.git_want(i);
        let s = self.st.git.entry(i.id.clone()).or_default();
        let usable = match &s.last {
            Some(Ok(Some(seen))) => seen.want == want,
            Some(_) => true,
            None => false,
        };
        if usable && (s.fresh || s.taken_in == seq) {
            s.fresh = false;
            s.taken_in = seq;
            return s.last.clone();
        }
        self.git_gather(i);
        None
    }

    /// The last facts gathered for `item`, fresh or not (what a projection may show).
    #[cfg(test)]
    pub(crate) fn git_seen(&self, item: &str) -> Option<&Seen> {
        match self.st.git.get(item)?.last.as_ref()? {
            Ok(Some(s)) => Some(s),
            _ => None,
        }
    }

    /// The rebase before a release of an item on a Node: done there off the lock, its outcome
    /// handled as the Hub's own rebase is (`rebase_for_release`); the release starts only if the
    /// rebase went through and the item still waits for that release.
    pub(crate) fn rebase_on_node(&mut self, item_id: &str, branch: &str, to: &To, decision: &str) {
        let Some(port) = self.c.node_git() else {
            return;
        };
        let (core, item, branch, to) = (
            self.c.clone(),
            item_id.to_string(),
            branch.to_string(),
            to.clone(),
        );
        /* one rebase per approval: the ledger takes the same operation once */
        let operation = format!("rebase-{decision}");
        detach(move || {
            let outcome = block_on(async {
                tokio::time::timeout(GATHER_DEADLINE, rebase(&*port, &item, &operation))
                    .await
                    .unwrap_or_else(|_| {
                        crate::git::Rebase::Error("结果未知（超时），不会自动重试".into())
                    })
            })
            .unwrap_or_else(|| crate::git::Rebase::Error("no runtime".into()));
            let mut t = core.tx();
            match t.rebased(&item, &branch, &to, outcome) {
                Ok(true) => {
                    if let Err(e) = t.start_release(&branch, &item) {
                        tracing::error!(item, "release after the rebase: {e}");
                    }
                }
                Ok(false) => {}
                Err(e) => tracing::error!(item, "rebase before the release: {e}"),
            }
        });
    }
}

/// The per-item git state the core keeps.
pub(crate) type Slots = HashMap<String, Slot>;
