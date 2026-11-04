//! Driving a lane: what [`super::store`] says has to be done is done here, off the core's lock (git and
//! the lane's checks take time), and the outcome recorded back, which may leave more to do.
//!
//! - a unit about to merge first asks you once (one card per batch, `approve` / `delivery_batch`); yes
//!   merges it, no sends its members back;
//! - merging: fetch, integrate on the target's tip ([`super::exec::integrate`]), the lane's checks,
//!   push fast-forward; a target that moved meanwhile is integrated again (twice at most);
//! - releasing: the repository's own release step ([`super::exec::release`]); the items then close by
//!   their project's completion rule (merged and tagged), as for any merge.
//!
//! Each item in a lane is told on its timeline where it is.

use std::path::{Path, PathBuf};

use steve_store::lanes::{self, LaneRequest, NewRequest};

use super::exec::{self, Member};
use super::store::{self, Work};
use crate::error::{Result, msg};
use crate::repo::{Item, NewDecision};
use crate::{Core, detach};

/// How many times a merge is integrated again when the target moved before its push.
const MOVED_RETRIES: usize = 2;

/// The units being merged or released in this process: one thread per unit, whatever triggers it
/// again meanwhile (a tick, a restart's recovery, a second answer).
static BUSY: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());

/// Holds a unit for one thread; `None` when another thread has it.
struct Hold((String, u64));

impl Hold {
    fn take(lane: &str, unit: u64) -> Option<Self> {
        let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        let k = (lane.to_string(), unit);
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

/// `subject_id` of a batch's approval card.
fn subject(lane: &str, unit: u64) -> String {
    format!("lane:{lane}:{unit}")
}

/// The (lane, unit) of a batch approval card's `subject_id`.
pub fn parse_subject(s: &str) -> Option<(String, u64)> {
    let rest = s.strip_prefix("lane:")?;
    let (lane, unit) = rest.rsplit_once(':')?;
    Some((lane.into(), unit.parse().ok()?))
}

fn label(r: &LaneRequest) -> String {
    match &r.item_id {
        Some(i) => format!("{i}（{}）", r.branch),
        None => r.branch.clone(),
    }
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
                    "merging" => format!("现在没有别的合并，直接合并（{unit}）"),
                    "collecting" => format!("和同一目标的其他合并一起成批（{unit}）"),
                    _ => "排在当前批次之后".into(),
                }
            ),
        );
        self.lane_run(work);
        Ok(req)
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
                Work::Merge {
                    lane,
                    unit,
                    requests,
                } => {
                    if let Err(e) = self.lane_ask(&lane, unit, &requests) {
                        tracing::warn!(lane, unit, "lane: approval card: {e}");
                    }
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

    /// The batch's one approval card.
    fn lane_ask(&self, lane: &str, unit: u64, ids: &[String]) -> Result<()> {
        let reqs = self.requests(ids);
        let ws = reqs
            .iter()
            .find_map(|r| r.workspace_id.clone())
            .unwrap_or_else(|| crate::repo::WS.into());
        let names: Vec<String> = reqs.iter().map(label).collect();
        self.store().decision_create(NewDecision {
            workspace_id: ws,
            kind: "approve".into(),
            subject_type: "delivery_batch".into(),
            subject_id: subject(lane, unit),
            question: format!("合并 MQ-{unit}（{} 项）", reqs.len()),
            draft: Some(format!(
                "这一批一起合并、一起检查、一起发布：\n{}",
                names
                    .iter()
                    .map(|n| format!("- {n}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )),
            options: Some(vec!["yes".into(), "no".into()]),
            requested_by: "steve".into(),
            ..Default::default()
        })?;
        for r in &reqs {
            self.lane_note(r, &format!("MQ-{unit} 等你批准合并"));
        }
        Ok(())
    }

    /// You answered a batch's card: yes merges it, no sends its members back.
    pub(crate) fn lane_answered(&self, lane: &str, unit: u64, yes: bool) {
        if yes {
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
        let Some(_hold) = Hold::take(lane, unit) else {
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
            .map(|r| PathBuf::from(&r.dir))
            .find(|d| d.is_dir())
        else {
            for r in &reqs {
                self.lane_left(lane, unit, r, "工作目录已经不在了");
            }
            return;
        };
        let checks: Vec<String> = serde_json::from_str(&row.checks).unwrap_or_default();
        let wt = self.worktree(lane, unit);
        let branch = format!("steve/merge/{}/MQ-{unit}", row.branch);
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
                    dir: r.dir.clone(),
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
            if let Err(e) = exec::checks(&wt, &checks) {
                // which member broke it is stage 3's bisect; until then the whole batch goes back
                for r in &reqs {
                    self.lane_left(lane, unit, r, &format!("整批{e}"));
                }
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
        let Some(_hold) = Hold::take(lane, unit) else {
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
                .map(|r| PathBuf::from(&r.dir))
                .find(|d| d.is_dir())
        };
        // released already (a restart came between the release and its record): not again
        let heads: Vec<String> = reqs.iter().map(|r| r.head.clone()).collect();
        let done = dir.as_deref().and_then(|d| {
            let _ = crate::upstream::fetch(d);
            exec::already_released(d, &heads)
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
    }

    /// A request left its lane: the item is reopened with a card saying why, as for a failed release.
    fn lane_sent_back(&self, item: &str, why: &str) -> Result<()> {
        self.tx()
            .release_failed(item, "合并通道退回", why, serde_json::json!({"lane": true}))
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
            let _ = self.store().store().transaction(|tx| {
                lanes::unit_set(tx, lane, unit as i64, "releasing", None, None, None)
            });
            self.lane_release(lane, unit, &ids);
            return;
        }
        match store::gave_up(self.store().store(), lane, unit) {
            Ok(w) => self.lane_run(w),
            Err(e) => tracing::warn!(lane, unit, "lane: gave up: {e}"),
        }
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
                            .reader(|c| lanes::approval_of(c, &subject(&l.id, no)))
                            .ok()
                            .flatten();
                        match card {
                            Some((status, true)) if status == "resolved" => {
                                let (c, lane) = (self.clone(), l.id.clone());
                                detach(move || c.lane_merge(&lane, no));
                            }
                            Some(_) => {} // waits for you, or was turned down (handled then)
                            None => {
                                let ids: Vec<String> = reqs
                                    .iter()
                                    .filter(|r| r.unit == Some(u.no) && r.state == "merging")
                                    .map(|r| r.id.clone())
                                    .collect();
                                if !ids.is_empty()
                                    && let Err(e) = self.lane_ask(&l.id, no, &ids)
                                {
                                    tracing::warn!(
                                        lane = l.id,
                                        unit = no,
                                        "lane recover: card: {e}"
                                    );
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
                    _ => {}
                }
            }
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
