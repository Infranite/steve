//! Driving a lane: what [`super::store`] says has to be done is done here, off the core's lock (git and
//! the lane's checks take time), and the outcome recorded back, which may leave more to do.
//!
//! - a unit about to merge first asks you once (one card per batch, `approve` / `delivery_batch`); yes
//!   merges it, no sends its members back; what arrives before you answer joins it, and a new card
//!   listing all of them takes the place of the open one (an answer is always to the list that merges);
//! - the target's host is asked before the card and again right before the push: a branch that takes
//!   changes only through a pull request or a merge queue is never pushed to directly (an owner allowed
//!   to bypass that would skip its reviews and checks). Its members leave the lane and you are told,
//!   with a card per item: their agents cannot change that rule, so they are not sent back to them;
//! - merging: fetch, integrate on the target's tip ([`super::exec::integrate`]),
//!   push fast-forward; a target that moved meanwhile is integrated again (twice at most). The lane
//!   runs no checks of its own: each member passed its own build when it was delivered, and the
//!   release tag's CI runs on the result;
//! - releasing: the repository's own release step ([`super::exec::release`]); the items then close by
//!   their project's completion rule (merged and tagged), as for any merge;
//! - a unit that is over (released, given up, emptied, or merged with its release put off) has its
//!   worktree and batch branch removed; a failed release keeps them for its retry.
//!
//! Each item in a lane is told on its timeline where it is.

use std::path::{Path, PathBuf};

use steve_store::lanes::{self, LaneRequest, NewRequest};

use super::exec::{self, Member};
use super::profile::{How, Merge};
use super::store::{self, Work, label, subject};
use super::target::Target;
use crate::error::{Result, msg};
use crate::repo::{Item, NewDecision};
use crate::{Core, detach};

/// How many times a merge is integrated again when the target moved before its push.
const MOVED_RETRIES: usize = 2;

/// The units being merged or released in this process: one thread per unit, whatever triggers it
/// again meanwhile (a tick, a restart's recovery, a second answer).
static BUSY: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());

/// Holds a unit's merge or its release (`phase`) for one thread; `None` when another thread has it.
/// Merging and releasing hold apart: a merge that just ended starts its unit's release while holding.
struct Hold((String, u64));

impl Hold {
    fn take(phase: &str, lane: &str, unit: u64) -> Option<Self> {
        let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        let k = (format!("{phase}:{lane}"), unit);
        if b.contains(&k) {
            return None;
        }
        b.push(k.clone());
        Some(Self(k))
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        BUSY.lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k| *k != self.0);
    }
}

/// The (lane, unit) of a batch approval card's `subject_id`.
pub fn parse_subject(s: &str) -> Option<(String, u64)> {
    let rest = s.strip_prefix("lane:")?;
    let (lane, unit) = rest.rsplit_once(':')?;
    Some((lane.into(), unit.parse().ok()?))
}

impl Core {
    /// An accepted item of a project whose completion rule is merge or tag asks to be merged: its
    /// delivered commit goes into the lane of its target. Returns the request.
    pub fn lane_enqueue_item(&self, item: &Item, repo: &str) -> Result<LaneRequest> {
        let branch = item.branch.clone().unwrap_or_default();
        // its worktree when it is there, else the project's repository and the item's branch
        let wt = item.cwd.as_deref().filter(|c| Path::new(c).is_dir());
        let cwd = wt.unwrap_or(repo);
        let dir = Path::new(cwd);
        let target = super::target::target_of(dir, None).map_err(msg)?;
        let rev = if wt.is_some() {
            "HEAD"
        } else {
            branch.as_str()
        };
        let head = crate::git::git(dir, &["rev-parse", rev]).map_err(msg)?;
        // published again: the same request, with what it now has (once it merges, it is what merged)
        let s = self.store().store();
        if let Some(r) = s.reader(|c| lanes::request_of_item(c, &item.id))? {
            if r.head != head && matches!(r.state.as_str(), "queued" | "collecting") {
                s.transaction(|tx| lanes::request_head(tx, &r.id, &head))?;
                self.lane_note(&r, "合并通道里的这一项换成了最新交付的提交");
            }
            return Ok(s.reader(|c| lanes::request_get(c, &r.id))?.unwrap_or(r));
        }
        let (req, work) = store::request(
            self.store().store(),
            &target.upstream,
            &target.branch,
            &NewRequest {
                lane_id: "",
                workspace_id: Some(&item.workspace_id),
                item_id: Some(&item.id),
                source: "item",
                dir: cwd,
                branch: &branch,
                head: &head,
                repo: Some(repo),
            },
        )?;
        let unit = req
            .unit
            .map(|u| format!("MQ-{u}"))
            .unwrap_or_else(|| "下一批".into());
        self.lane_note(
            &req,
            &format!(
                "进入合并通道（{} {}），{}",
                target.upstream,
                target.branch,
                match req.state.as_str() {
                    // nothing to start: it went in with a unit whose card waits for you
                    "merging" if work.is_empty() => {
                        format!("并入还在等你批准的 {unit}，批准卡换成一起列出它的新卡")
                    }
                    "merging" => format!("现在没有别的合并，直接合并（{unit}）"),
                    "collecting" => format!("和同一目标的其他合并一起成批（{unit}）"),
                    _ => "排在当前批次之后".into(),
                }
            ),
        );
        self.lane_run(work);
        Ok(req)
    }

    /// What the project page shows of the lane of (`upstream`, `branch`) ([`store::view`]).
    pub fn lane_view(&self, upstream: &str, branch: &str) -> Result<Option<serde_json::Value>> {
        Ok(store::view(self.store().store(), upstream, branch)?)
    }

    /// Your limits for the lane of (`upstream`, `branch`) (`None`: the default); the lane as shown.
    pub fn lane_policy(
        &self,
        upstream: &str,
        branch: &str,
        window_ms: Option<i64>,
        max_members: Option<i64>,
    ) -> Result<Option<serde_json::Value>> {
        let s = self.store().store();
        store::set_policy(s, upstream, branch, window_ms, max_members)?;
        // a shorter window may be over already
        self.lane_run(store::tick(s)?);
        self.lane_view(upstream, branch)
    }

    /// "Merge and release now" on the lane of (`upstream`, `branch`): its collecting batch closes and
    /// starts as soon as nothing is in flight (its approval card still asks you). `false`: there was
    /// no batch collecting.
    pub fn lane_now(&self, upstream: &str, branch: &str) -> Result<bool> {
        let s = self.store().store();
        let Some(l) = s
            .reader(lanes::lanes_all)?
            .into_iter()
            .find(|l| l.upstream == upstream && l.branch == branch)
        else {
            return Ok(false);
        };
        let (closed, work) = store::now(s, &l.id)?;
        if closed {
            for r in s
                .reader(|c| lanes::requests_of(c, &l.id, true))?
                .iter()
                .filter(|r| r.state == "collecting")
            {
                self.lane_note(r, "你要求立即合并发布：这一批不再等合批窗口");
            }
        }
        self.lane_run(work);
        Ok(closed)
    }

    /// Time passed: lanes whose window is due close and start.
    pub fn lane_tick(&self) -> Result<()> {
        let work = store::tick(self.store().store())?;
        self.lane_run(work);
        Ok(())
    }

    /// Do what is left to do.
    pub(crate) fn lane_run(&self, work: Vec<Work>) {
        for w in work {
            match w {
                Work::Merge { lane, unit, .. } => {
                    // the host's rules are asked first (`gh`, off the lock)
                    let c = self.clone();
                    detach(move || c.lane_ask(&lane, unit));
                }
                Work::Release {
                    lane,
                    unit,
                    requests,
                } => {
                    let c = self.clone();
                    detach(move || c.lane_release(&lane, unit, &requests));
                }
            }
        }
    }

    fn requests(&self, ids: &[String]) -> Vec<LaneRequest> {
        ids.iter()
            .filter_map(|id| {
                self.store()
                    .store()
                    .reader(|c| lanes::request_get(c, id))
                    .ok()
                    .flatten()
            })
            .collect()
    }

    /// The batch's one approval card, listing what the unit holds now (what joins it until you answer
    /// gets a new card listing all of them: [`store`]); none when its target takes no direct push.
    fn lane_ask(&self, lane: &str, unit: u64) {
        let s = self.store().store();
        let Ok(Some(row)) = s.reader(|c| lanes::lane_get(c, lane)) else {
            return;
        };
        if let Some(why) = self.host_refuses(&row) {
            let reqs = s
                .reader(|c| store::merging(c, lane, unit))
                .unwrap_or_default();
            for r in &reqs {
                self.lane_refused(lane, unit, r, &why);
            }
            return;
        }
        match store::ask(s, lane, unit) {
            Ok(reqs) => {
                for r in &reqs {
                    self.lane_note(r, &format!("MQ-{unit} 等你批准合并"));
                }
            }
            Err(e) => tracing::warn!(lane, unit, "lane: approval card: {e}"),
        }
    }

    /// Why the lane's target takes no direct push: its host requires a pull request or its merge
    /// queue (as found, not assumed). `None`: it may be pushed to.
    fn host_refuses(&self, row: &lanes::LaneRow) -> Option<String> {
        let target = Target {
            upstream: row.upstream.clone(),
            branch: row.branch.clone(),
        };
        let merge = super::profile::merge_of(&target, &self.config().gh);
        if merge.how != How::Found || merge.value == Merge::Push {
            return None;
        }
        let how = if merge.value == Merge::HostQueue {
            "托管方的合并队列"
        } else {
            "PR"
        };
        Some(format!(
            "目标分支 {} 规定只能经{how}合入，合并通道不直接推送到它。执行者改不了这条规则，所以没有退回给它：可以改用 PR 合并，或在托管方调整分支规则后再试",
            row.branch
        ))
    }

    /// A request leaves its lane for a reason only you can change: you are told with a card on the
    /// item; its agent is not sent back to work.
    fn lane_refused(&self, lane: &str, unit: u64, r: &LaneRequest, why: &str) {
        self.lane_note(r, &format!("退出 MQ-{unit}：{why}"));
        match store::left(self.store().store(), lane, unit, &r.id, why) {
            Ok(w) => self.lane_run(w),
            Err(e) => tracing::warn!(lane, unit, "lane: left: {e}"),
        }
        self.lane_sweep(lane);
        if let Some(i) = &r.item_id
            && let Err(e) = self.tx().release_failed(
                i,
                "合并通道没有合并，等你处理",
                &format!("合并通道没有合并：{why}"),
                serde_json::json!({"lane": true}),
            )
        {
            tracing::warn!(item = i, "lane: refused: {e}");
        }
    }

    /// You answered a batch's card: yes merges it, no sends its members back.
    pub(crate) fn lane_answered(&self, lane: &str, unit: u64, yes: bool) {
        if yes {
            // under way from here: what arrives now waits for the next batch
            if let Err(e) = store::approved(self.store().store(), lane, unit) {
                tracing::warn!(lane, unit, "lane: approved: {e}");
            }
            let (c, lane) = (self.clone(), lane.to_string());
            detach(move || c.lane_merge(&lane, unit));
            return;
        }
        let reqs = self
            .store()
            .store()
            .reader(|c| lanes::requests_of(c, lane, true))
            .unwrap_or_default();
        for r in reqs
            .iter()
            .filter(|r| r.unit == Some(unit as i64) && r.state == "merging")
        {
            self.lane_left(lane, unit, r, "你没有批准这一批");
        }
    }

    fn lane_left(&self, lane: &str, unit: u64, r: &LaneRequest, why: &str) {
        self.lane_note(r, &format!("退出 MQ-{unit}：{why}"));
        match store::left(self.store().store(), lane, unit, &r.id, why) {
            Ok(w) => self.lane_run(w),
            Err(e) => tracing::warn!(lane, unit, "lane: left: {e}"),
        }
        self.lane_sweep(lane);
        // the item goes back to its agent, as a failed release does: reopened, a card says why
        if let Some(i) = &r.item_id
            && let Err(e) = self.lane_sent_back(i, &format!("合并通道 MQ-{unit} 退回：{why}"))
        {
            tracing::warn!(item = i, "lane: sent back: {e}");
        }
    }

    fn worktree(&self, lane: &str, unit: u64) -> PathBuf {
        self.config()
            .home
            .join("worktrees/lanes")
            .join(lane)
            .join(format!("MQ-{unit}"))
    }

    /// Merge unit `unit` of `lane` (off the lock).
    fn lane_merge(&self, lane: &str, unit: u64) {
        let Some(_hold) = Hold::take("merge", lane, unit) else {
            return;
        };
        let s = self.store().store();
        let Ok(Some(row)) = s.reader(|c| lanes::lane_get(c, lane)) else {
            return;
        };
        let mut reqs: Vec<LaneRequest> = s
            .reader(|c| lanes::requests_of(c, lane, true))
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.unit == Some(unit as i64) && r.state == "merging")
            .collect();
        let Some(repo) = reqs
            .iter()
            .flat_map(|r| [Some(r.dir.clone()), r.repo.clone()])
            .flatten()
            .map(PathBuf::from)
            .find(|d| d.is_dir())
        else {
            for r in &reqs {
                self.lane_left(lane, unit, r, "工作目录已经不在了");
            }
            return;
        };
        let wt = self.worktree(lane, unit);
        let branch = format!("steve/merge/{}/MQ-{unit}", row.branch);
        // asked again right before the push (the rules may have changed since the card): a branch whose
        // host takes changes only through a pull request or its merge queue is not pushed to; with the
        // rights to bypass it, a push would skip the reviews and checks it asks for
        if let Some(why) = self.host_refuses(&row) {
            for r in &reqs {
                self.lane_refused(lane, unit, r, &why);
            }
            return;
        }
        for attempt in 0..=MOVED_RETRIES {
            if let Err(e) = crate::upstream::fetch(&repo) {
                for r in &reqs {
                    self.lane_left(lane, unit, r, &format!("取不到目标的最新状态：{e}"));
                }
                return;
            }
            let base =
                match crate::git::git(&repo, &["rev-parse", &format!("origin/{}", row.branch)]) {
                    Ok(b) => b,
                    Err(e) => {
                        for r in &reqs {
                            self.lane_left(lane, unit, r, &format!("找不到目标分支：{e}"));
                        }
                        return;
                    }
                };
            let members: Vec<Member> = reqs
                .iter()
                .map(|r| Member {
                    id: r.id.clone(),
                    head: r.head.clone(),
                    label: label(r),
                    // its worktree, else its project's repository: the branch and commit live in both
                    dir: [Some(&r.dir), r.repo.as_ref()]
                        .into_iter()
                        .flatten()
                        .find(|d| Path::new(d).is_dir())
                        .cloned()
                        .unwrap_or_default(),
                    branch: r.branch.clone(),
                })
                .collect();
            let done = match exec::integrate(&repo, &wt, &branch, &base, &members) {
                Ok(d) => d,
                Err(e) => {
                    for r in &reqs {
                        self.lane_left(lane, unit, r, &format!("建不了批次分支：{e}"));
                    }
                    return;
                }
            };
            for (id, why) in &done.left {
                if let Some(r) = reqs.iter().find(|r| &r.id == id) {
                    self.lane_left(lane, unit, r, why);
                }
            }
            reqs.retain(|r| done.merged.contains(&r.id));
            if reqs.is_empty() {
                return;
            }
            match crate::upstream::push_head(&wt, &row.branch) {
                Ok(()) => {
                    let _ = s.transaction(|tx| {
                        lanes::unit_set(tx, lane, unit as i64, "merging", Some(&branch), None, None)
                    });
                    for r in &reqs {
                        self.lane_note(r, &format!("MQ-{unit} 已合并到 {}", row.branch));
                    }
                    match store::merged(s, lane, unit) {
                        Ok(w) => self.lane_run(w),
                        Err(e) => tracing::warn!(lane, unit, "lane: merged: {e}"),
                    }
                    // its release put off into the next batch: this unit is over
                    self.lane_sweep(lane);
                    return;
                }
                Err(e) if attempt < MOVED_RETRIES => {
                    tracing::info!(lane, unit, "lane: target moved, integrating again: {e}");
                }
                Err(e) => {
                    for r in &reqs {
                        self.lane_left(lane, unit, r, &format!("目标一直在变，推不上去：{e}"));
                    }
                    return;
                }
            }
        }
    }

    /// Release unit `unit` of `lane` (off the lock).
    fn lane_release(&self, lane: &str, unit: u64, ids: &[String]) {
        let Some(_hold) = Hold::take("release", lane, unit) else {
            return;
        };
        let s = self.store().store();
        let Ok(Some(row)) = s.reader(|c| lanes::lane_get(c, lane)) else {
            return;
        };
        let reqs = self.requests(ids);
        let wt = self.worktree(lane, unit);
        let dir = if wt.is_dir() {
            Some(wt)
        } else {
            reqs.iter()
                .flat_map(|r| [Some(r.dir.clone()), r.repo.clone()])
                .flatten()
                .map(PathBuf::from)
                .find(|d| d.is_dir())
        };
        // released already (a restart came between the release and its record): not again
        let heads: Vec<String> = reqs.iter().map(|r| r.head.clone()).collect();
        let done = dir.as_deref().and_then(|d| {
            crate::upstream::fetch(d).ok()?;
            let tags = crate::upstream::remote_tags(d).ok()?;
            exec::already_released(d, &heads, &tags)
        });
        let result = match done {
            Some(v) => Some(Ok(Some(v))),
            None => dir.map(|d| exec::release(&d, &row.branch)),
        };
        let (outcome, version) = match result {
            Some(Ok(v)) => ("ok", v),
            Some(Err(e)) => {
                self.lane_release_failed(lane, unit, &reqs, &e);
                return;
            }
            None => {
                self.lane_release_failed(lane, unit, &reqs, "找不到可以发布的仓库目录");
                return;
            }
        };
        if outcome == "ok" {
            for r in &reqs {
                self.lane_note(
                    r,
                    &match &version {
                        Some(v) => format!("MQ-{unit} 已发布为 v{v}"),
                        None => format!("MQ-{unit} 已合并，这个仓库合并即发布"),
                    },
                );
            }
        }
        match store::released(s, lane, unit, outcome, version.as_deref()) {
            Ok(w) => self.lane_run(w),
            Err(e) => tracing::warn!(lane, unit, "lane: released: {e}"),
        }
        self.lane_sweep(lane);
    }

    /// The worktrees and batch branches of `lane`'s units that are over (state `done`: released, given
    /// up, emptied, or merged with the release put off) go; a unit still merging, releasing or
    /// waiting for a retry of its failed release keeps them. By state, never by age.
    pub(crate) fn lane_sweep(&self, lane: &str) {
        let s = self.store().store();
        let (Ok(Some(row)), Ok(units)) = (
            s.reader(|c| lanes::lane_get(c, lane)),
            s.reader(|c| lanes::units_of(c, lane)),
        ) else {
            return;
        };
        for u in units.iter().filter(|u| u.state == "done") {
            let wt = self.worktree(lane, u.no as u64);
            if !wt.exists() {
                continue;
            }
            let branch = u
                .branch
                .clone()
                .unwrap_or_else(|| format!("steve/merge/{}/MQ-{}", row.branch, u.no));
            if let Err(e) = remove_worktree(&wt, &branch) {
                tracing::warn!(lane, unit = u.no, "lane: worktree not removed: {e}");
            }
        }
    }

    /// A request left its lane: the item is reopened with a card saying why, as for a failed release.
    fn lane_sent_back(&self, item: &str, why: &str) -> Result<()> {
        let mut t = self.tx();
        t.release_outcome(item, "failed", serde_json::json!({"lane": true}))?;
        // the accepted result is void: back to work (the acceptance expires), its session told why
        if !t.lc_invalidate(item, why)? {
            return t.release_failed(item, "合并通道退回", why, serde_json::json!({"lane": true}));
        }
        match t.active_session(item)? {
            Some(s) => {
                t.prompt(
                    &s.id,
                    &format!("合并通道退回：{why}。请处理后重新交付，通过后会再进合并通道。"),
                    "steve",
                );
                Ok(())
            }
            None => t.fail(item, why),
        }
    }

    /// A unit's release failed: its members wait with it, a card asks whether to try again or give up.
    fn lane_release_failed(&self, lane: &str, unit: u64, reqs: &[LaneRequest], why: &str) {
        tracing::warn!(lane, unit, "lane: release failed: {why}");
        for r in reqs {
            self.lane_note(r, &format!("MQ-{unit} 发布失败：{why}"));
        }
        if let Err(e) = store::release_failed(self.store().store(), lane, unit, why) {
            tracing::warn!(lane, unit, "lane: release failed: {e}");
        }
        self.lane_fail_card(lane, unit, reqs, why);
    }

    /// The card asking what next for a unit whose release failed: retry, or give up.
    fn lane_fail_card(&self, lane: &str, unit: u64, reqs: &[LaneRequest], why: &str) {
        let ws = reqs
            .iter()
            .find_map(|r| r.workspace_id.clone())
            .unwrap_or_else(|| crate::repo::WS.into());
        if let Err(e) = self.store().decision_create(NewDecision {
            workspace_id: ws,
            kind: "fail".into(),
            subject_type: "delivery_batch".into(),
            subject_id: subject(lane, unit),
            question: format!("MQ-{unit} 已合并，但发布失败"),
            draft: Some(why.to_string()),
            options: Some(vec!["retry".into(), "drop".into()]),
            requested_by: "steve".into(),
            ..Default::default()
        }) {
            tracing::warn!(lane, unit, "lane: release failed card: {e}");
        }
    }

    /// Your answer on a failed release: try it again, or give up (the lane moves on).
    pub(crate) fn lane_release_answered(&self, lane: &str, unit: u64, retry: bool) {
        let ids: Vec<String> = self
            .store()
            .store()
            .reader(|c| lanes::requests_of(c, lane, true))
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.released_in == Some(unit as i64))
            .map(|r| r.id)
            .collect();
        if retry {
            // the answer is used up with the step it allows: a restart after this does not retry again
            let _ = self.store().store().transaction(|tx| {
                lanes::unit_set(tx, lane, unit as i64, "releasing", None, None, None)?;
                lanes::batch_card_acted(tx, &subject(lane, unit), "fail")
            });
            self.lane_release(lane, unit, &ids);
            return;
        }
        match store::gave_up(self.store().store(), lane, unit) {
            Ok(w) => self.lane_run(w),
            Err(e) => tracing::warn!(lane, unit, "lane: gave up: {e}"),
        }
        self.lane_sweep(lane);
    }

    /// After a restart: batches you approved that did not finish merging merge again (against the
    /// target's tip again, so nothing doubles); releases that did not finish release, unless the commits
    /// already went out; a batch without its card gets it. Idempotent: units already being driven in
    /// this process are left alone.
    pub fn lane_recover(&self) {
        let s = self.store().store();
        let Ok(all) = s.reader(lanes::lanes_all) else {
            return;
        };
        for l in all {
            let Ok(units) = s.reader(|c| lanes::units_of(c, &l.id)) else {
                continue;
            };
            let reqs = s
                .reader(|c| lanes::requests_of(c, &l.id, true))
                .unwrap_or_default();
            for u in units {
                let no = u.no as u64;
                match u.state.as_str() {
                    "merging" => {
                        let card = s
                            .reader(|c| lanes::batch_card(c, &subject(&l.id, no), "approve"))
                            .ok()
                            .flatten();
                        match card {
                            // answered, maybe not acted on before the restart: act on it again
                            Some((status, result)) if status == "resolved" => {
                                let yes = result["approved"].as_bool() == Some(true);
                                let (c, lane) = (self.clone(), l.id.clone());
                                detach(move || c.lane_answered(&lane, no, yes));
                            }
                            Some(_) => {} // waits for you
                            None => {
                                let any = reqs
                                    .iter()
                                    .any(|r| r.unit == Some(u.no) && r.state == "merging");
                                if any {
                                    let (c, lane) = (self.clone(), l.id.clone());
                                    detach(move || c.lane_ask(&lane, no));
                                }
                            }
                        }
                    }
                    "releasing" => {
                        let ids: Vec<String> = reqs
                            .iter()
                            .filter(|r| r.released_in == Some(u.no))
                            .map(|r| r.id.clone())
                            .collect();
                        let (c, lane) = (self.clone(), l.id.clone());
                        detach(move || c.lane_release(&lane, no, &ids));
                    }
                    "release_failed" => {
                        let card = s
                            .reader(|c| lanes::batch_card(c, &subject(&l.id, no), "fail"))
                            .ok()
                            .flatten();
                        match card {
                            // answered and not yet acted on (an acted retry failed again: a new card)
                            Some((status, result))
                                if status == "resolved"
                                    && result["acted"].as_bool() != Some(true) =>
                            {
                                let retry = result["choice"].as_str() == Some("retry");
                                let (c, lane) = (self.clone(), l.id.clone());
                                detach(move || c.lane_release_answered(&lane, no, retry));
                            }
                            Some((status, _)) if status == "open" => {} // waits for you
                            // none yet, or the last answer was used up by a retry that failed again
                            _ => {
                                let mine: Vec<LaneRequest> = reqs
                                    .iter()
                                    .filter(|r| r.released_in == Some(u.no))
                                    .cloned()
                                    .collect();
                                let why = u.outcome.as_deref().unwrap_or("发布失败");
                                self.lane_fail_card(&l.id, no, &mine, why);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // what a unit that ended before this left behind
            self.lane_sweep(&l.id);
        }
    }

    /// A line on the timeline of the request's item, if it has one.
    fn lane_note(&self, r: &LaneRequest, text: &str) {
        if let Some(i) = &r.item_id
            && let Err(e) = self.store().timeline_add(i, "steve", "sys", text, None)
        {
            tracing::warn!(item = i, "lane note: {e}");
        }
    }
}

/// Remove the worktree `wt` from the repository it belongs to, and its branch `branch` there.
fn remove_worktree(wt: &Path, branch: &str) -> std::result::Result<(), String> {
    let common = crate::git::git(
        wt,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let wt_s = wt.to_string_lossy();
    // only when `wt` is itself a worktree's top (not a stray directory inside some other repository)
    let top = crate::git::git(wt, &["rev-parse", "--show-toplevel"])
        .ok()
        .and_then(|t| PathBuf::from(t).canonicalize().ok());
    let ours = top.is_some() && top == wt.canonicalize().ok();
    let repo = common.ok().filter(|_| ours).map(PathBuf::from);
    if let Some(repo) = &repo {
        let _ = crate::git::git(repo, &["worktree", "remove", "--force", &wt_s]);
    }
    // not a worktree any more (its repository went), or git would not: the directory goes anyway
    if wt.exists() {
        std::fs::remove_dir_all(wt).map_err(|e| e.to_string())?;
    }
    if let Some(repo) = &repo {
        let _ = crate::git::git(repo, &["worktree", "prune"]);
        let _ = crate::git::git(repo, &["branch", "-D", branch]);
    }
    Ok(())
}
