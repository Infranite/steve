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
    pub fn lane_enqueue_item(&self, item: &Item) -> Result<LaneRequest> {
        let cwd = item
            .cwd
            .as_deref()
            .ok_or_else(|| msg("这个事项没有工作目录，不能进合并通道"))?;
        let dir = Path::new(cwd);
        let target = super::target::target_of(dir, None).map_err(msg)?;
        let head = crate::git::git(dir, &["rev-parse", "HEAD"]).map_err(msg)?;
        let branch = item.branch.clone().unwrap_or_default();
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
        let branch = format!("steve/merge/MQ-{unit}");
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
        let (outcome, version) = match dir.map(|d| exec::release(&d, &row.branch)) {
            Some(Ok(v)) => ("ok", v),
            Some(Err(e)) => {
                tracing::warn!(lane, unit, "lane: release failed: {e}");
                for r in &reqs {
                    self.lane_note(r, &format!("MQ-{unit} 发布失败：{e}"));
                }
                ("failed", None)
            }
            None => ("failed", None),
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

    /// A line on the timeline of the request's item, if it has one.
    fn lane_note(&self, r: &LaneRequest, text: &str) {
        if let Some(i) = &r.item_id
            && let Err(e) = self.store().timeline_add(i, "steve", "sys", text, None)
        {
            tracing::warn!(item = i, "lane note: {e}");
        }
    }
}
