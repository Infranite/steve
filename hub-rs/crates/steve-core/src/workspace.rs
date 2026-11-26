//! Task workspaces: disk policy, report models, and Node-delegated housekeeping.
//!
//! All filesystem scanning, process inspection (`lsof` / `/proc`), `.trash` staging/purging,
//! and git worktree pruning/tidying run on the colocated Node via [`NodeHousekeeping`]
//! (`steve-node/src/housekeeping.rs`). Core owns only item state, policy thresholds, and
//! recording timeline/notification outcomes.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use steve_node_api::{
    ApiResult, DiskInspectRequest, DiskInspectResponse, DiskReclaimRequest, DiskReclaimResponse,
    ReclaimCacheTarget, ReclaimClosedTarget,
};
pub use steve_toolchains::ITEM_BUILD_DIR;

use crate::error::{CoreError, Result};
use crate::util::now;
use crate::{Core, Tx};

pub const GB: u64 = 1 << 30;
/// How often the watcher asks the Node to reclaim disk space.
const CHECK_EVERY: Duration = Duration::from_secs(600);

fn cache_names() -> &'static [String] {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        steve_toolchains::Toolchains::builtin()
            .disposable_names()
            .into_iter()
            .map(String::from)
            .collect()
    })
}

/// Whether one real directory contains only regenerable toolchain/cache data.
/// Used by Hub backup traversal on Hub-owned staging/backup directories.
pub fn is_disposable_dir(dir: &Path, workspace_root: Option<&Path>) -> bool {
    fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        && (dir.parent().is_some_and(|p| match workspace_root {
            Some(root) => p == root.join(ITEM_BUILD_DIR),
            None => p.ends_with(ITEM_BUILD_DIR),
        }) || cache_names()
            .iter()
            .any(|n| dir.file_name().is_some_and(|name| name == n.as_str()))
            || dir.join("CACHEDIR.TAG").is_file())
}

/// Node-delegated workspace disk inspection and reclamation port.
#[async_trait]
pub trait NodeHousekeeping: Send + Sync {
    async fn disk_inspect(&self, req: DiskInspectRequest) -> ApiResult<DiskInspectResponse>;
    async fn disk_reclaim(&self, req: DiskReclaimRequest) -> ApiResult<DiskReclaimResponse>;
}

/// The disk thresholds, for the volume the worktrees are on (`STEVE_DISK_FLOOR_GB`, `STEVE_DISK_LOW_GB`,
/// `STEVE_WORKTREE_IDLE_HOURS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiskPolicy {
    /// Below this, no new worktree is made (bytes).
    pub floor: u64,
    /// Below this, idle items' caches are given back too (bytes).
    pub low: u64,
    /// An open item counts as idle after this long without a change (ms).
    pub idle_ms: i64,
}

impl Default for DiskPolicy {
    fn default() -> Self {
        DiskPolicy {
            floor: 10 * GB,
            low: 40 * GB,
            idle_ms: 24 * 3_600_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{var}: {message}")]
pub struct DiskPolicyError {
    pub var: &'static str,
    pub message: &'static str,
}

impl DiskPolicy {
    /// Unset values keep defaults; configured values must be finite, nonnegative and within range.
    pub fn from_vars(
        get: impl Fn(&str) -> Option<String>,
    ) -> std::result::Result<Self, DiskPolicyError> {
        let d = DiskPolicy::default();
        let num = |var: &'static str, scale: f64, max: f64| {
            get(var)
                .map(|value| {
                    value
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite() && *n >= 0.0)
                        .map(|n| n * scale)
                        .filter(|n| n.is_finite() && *n < max)
                        .ok_or(DiskPolicyError {
                            var,
                            message: "expected a finite nonnegative number within range",
                        })
                })
                .transpose()
        };
        let floor =
            num("STEVE_DISK_FLOOR_GB", GB as f64, u64::MAX as f64)?.map_or(d.floor, |n| n as u64);
        let low = match num("STEVE_DISK_LOW_GB", GB as f64, u64::MAX as f64)? {
            Some(n) if n < floor as f64 => {
                return Err(DiskPolicyError {
                    var: "STEVE_DISK_LOW_GB",
                    message: "must be at least STEVE_DISK_FLOOR_GB",
                });
            }
            Some(n) => n as u64,
            None => d.low.max(floor),
        };
        let idle_ms = num("STEVE_WORKTREE_IDLE_HOURS", 3_600_000.0, i64::MAX as f64)?
            .map_or(d.idle_ms, |n| n as i64);
        Ok(DiskPolicy {
            floor,
            low,
            idle_ms,
        })
    }

    pub fn pressure(&self, free: u64) -> Pressure {
        if free < self.floor {
            Pressure::Floor
        } else if free < self.low {
            Pressure::Low
        } else {
            Pressure::Ok
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Pressure {
    Ok,
    Low,
    Floor,
}

/// "12.3 GB" (binary units, as `df -h` and Finder's "GB" on the same disk round them alike enough).
pub fn human(bytes: u64) -> String {
    let g = bytes as f64 / GB as f64;
    if g >= 10.0 {
        format!("{g:.0} GB")
    } else if g >= 0.1 {
        format!("{g:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / (1u64 << 20) as f64)
    }
}

/// What one trash folder held once deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Freed {
    pub item: String,
    /// The caches, relative to the worktree (`hub-rs/target`).
    pub caches: Vec<String>,
    pub bytes: u64,
}

/// One pass of [`Core::reclaim_workspaces`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct Reclaimed {
    /// Another pass was running; this one did nothing.
    pub skipped: bool,
    pub free_before: Option<u64>,
    pub free_after: Option<u64>,
    pub freed: Vec<Freed>,
    /// Items whose worktree was removed.
    pub removed: Vec<String>,
    /// Worktrees left alone on purpose, and why.
    pub kept: Vec<Kept>,
}

impl Reclaimed {
    pub fn freed_bytes(&self) -> u64 {
        self.freed.iter().map(|f| f.bytes).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kept {
    pub item: String,
    pub reason: String,
}

/// One item's worktree as [`Core::disk_report`] shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub item: String,
    pub workspace: String,
    pub title: String,
    pub phase: String,
    pub archived: bool,
    pub branch: Option<String>,
    pub path: String,
    pub exists: bool,
    pub bytes: u64,
    pub cache_bytes: u64,
    pub caches: Vec<String>,
    /// A session of the item is working now.
    pub busy: bool,
    /// A process runs inside the worktree (`None`: processes could not be listed).
    pub in_use: Option<bool>,
    pub updated: i64,
}

/// A directory in the worktrees folder no item points at.
#[derive(Debug, Clone, Serialize)]
pub struct Orphan {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskReport {
    pub root: String,
    pub free: Option<u64>,
    pub total: Option<u64>,
    pub pressure: Option<Pressure>,
    pub policy: DiskPolicy,
    pub git: Option<String>,
    pub trash: u64,
    pub entries: Vec<Entry>,
    /// Items' scratch directories (items without a project); `caches` are what an ended item gives
    /// back under pressure.
    pub scratch: Vec<Entry>,
    pub orphans: Vec<Orphan>,
    pub measured_at: i64,
}

fn localize_kept_reason(code: &str) -> String {
    match code {
        "in_use" => "有进程还在工作区里运行".into(),
        "dirty" => "有未提交的改动，保留工作区".into(),
        "unmerged" => "分支还没合入主线，归档后回收工作区（分支保留）".into(),
        "remove_failed" => "工作区未能删除，下次再试".into(),
        "unknown_state" => "看不出工作区的状态，保留".into(),
        other => other.to_string(),
    }
}

impl Core {
    /// Wires the colocated Node's workspace housekeeping port (once).
    pub fn set_housekeeping(&self, h: Arc<dyn NodeHousekeeping>) {
        let _ = self.0.housekeeping.set(h);
    }

    pub(crate) fn node_housekeeping(&self) -> Option<Arc<dyn NodeHousekeeping>> {
        self.0.housekeeping.get().cloned()
    }

    fn record_free(&self, free: Option<u64>) {
        if let Some(f) = free {
            self.0.last_free_bytes.store(f, Ordering::Relaxed);
        }
    }

    /// Last observed free bytes on the worktrees volume, if known.
    pub(crate) fn worktrees_free(&self) -> Option<u64> {
        let v = self.0.last_free_bytes.load(Ordering::Relaxed);
        (v != u64::MAX).then_some(v)
    }

    /// Why no new worktree is made with `free` bytes left (below the floor), and a pass starts
    /// giving space back.
    pub(crate) fn disk_low(&self, free: u64) -> String {
        let c = self.clone();
        crate::detach(move || {
            if let Err(e) = c.reclaim_workspaces() {
                tracing::debug!("reclaim {e}");
            }
        });
        steve_i18n::t!(
            "core.workspace.disk_low",
            free = human(free),
            floor = human(self.config().disk.floor)
        )
    }

    /// Inspect a batch of worktree paths (and optional branch states) on the colocated Node.
    pub(crate) fn inspect_worktrees(
        &self,
        entries: Vec<steve_node_api::DiskInspectTarget>,
    ) -> Result<DiskInspectResponse> {
        let Some(port) = self.node_housekeeping() else {
            return Err(CoreError::NoNodeAvailable(
                "本地设备节点未连接，无法检查工作区状态".into(),
            ));
        };
        let req = DiskInspectRequest {
            worktrees_root: self.config().worktrees.to_string_lossy().into_owned(),
            entries,
            scratch: vec![],
        };
        crate::node_git::block_on(port.disk_inspect(req))
            .ok_or_else(|| CoreError::Msg("workspace disk inspect runtime unavailable".into()))?
            .map_err(|e| {
                if e.code == steve_node_api::Code::Unavailable {
                    CoreError::NoNodeAvailable(format!("本地设备节点离线：{}", e.safe_detail))
                } else {
                    CoreError::Msg(e.safe_detail)
                }
            })
    }

    /// Ask the colocated Node whether `branch` in `repo` is merged into its default base and what
    /// its short head SHA is (`None` head means the branch no longer exists).
    pub(crate) fn inspect_branch(
        &self,
        repo: &str,
        branch: &str,
    ) -> Option<(bool, Option<String>)> {
        if repo.is_empty() || branch.is_empty() {
            return None;
        }
        let resp = self
            .inspect_worktrees(vec![steve_node_api::DiskInspectTarget {
                item: String::new(),
                path: String::new(),
                repo: repo.to_string(),
                branch: branch.to_string(),
            }])
            .ok()?;
        let m = resp.entries.into_iter().next()?;
        m.branch_merged.map(|merged| (merged, m.branch_head))
    }

    /// Deterministically reclaim a single closed item's worktree and branch on the colocated Node.
    pub(crate) fn reclaim_single_closed(
        &self,
        target: ReclaimClosedTarget,
    ) -> Result<DiskReclaimResponse> {
        let Some(port) = self.node_housekeeping() else {
            return Err(CoreError::NoNodeAvailable(
                "本地设备节点未连接，无法清理工作区".into(),
            ));
        };
        let req = DiskReclaimRequest {
            worktrees_root: self.config().worktrees.to_string_lossy().into_owned(),
            git_projects: vec![],
            closed_worktrees: vec![target],
            idle_worktrees: vec![],
            ended_scratch: vec![],
            low_bytes: u64::MAX,
        };
        crate::node_git::block_on(port.disk_reclaim(req))
            .ok_or_else(|| CoreError::Msg("workspace reclaim runtime unavailable".into()))?
            .map_err(|e| {
                if e.code == steve_node_api::Code::Unavailable {
                    CoreError::NoNodeAvailable(format!("本地设备节点离线：{}", e.safe_detail))
                } else {
                    CoreError::Msg(e.safe_detail)
                }
            })
    }

    /// One pass on the colocated Node: deletes trash, reclaims closed worktrees/caches, and under
    /// pressure sheds idle worktree and ended scratch caches until free space exceeds `DiskPolicy::low`.
    /// One pass at a time; a second caller gets `skipped`.
    pub fn reclaim_workspaces(&self) -> Result<Reclaimed> {
        if self.0.reclaiming.swap(true, Ordering::SeqCst) {
            return Ok(Reclaimed {
                skipped: true,
                ..Default::default()
            });
        }
        let r = self.reclaim_pass();
        self.0.reclaiming.store(false, Ordering::SeqCst);
        let (reclaimed, resp) = r?;
        self.record_free(reclaimed.free_after.or(reclaimed.free_before));
        let t = self.tx();
        t.note_freed(&resp.freed)?;
        t.note_reclaim_outcomes(&resp)?;
        Ok(reclaimed)
    }

    fn reclaim_pass(&self) -> Result<(Reclaimed, DiskReclaimResponse)> {
        let Some(port) = self.node_housekeeping() else {
            return Err(CoreError::NoNodeAvailable(
                "本地设备节点未连接，无法执行工作区回收".into(),
            ));
        };
        let (req, mut extra_kept) = self.tx().build_reclaim_request()?;
        let resp = crate::node_git::block_on(port.disk_reclaim(req))
            .ok_or_else(|| CoreError::Msg("workspace reclaim runtime unavailable".into()))?
            .map_err(|e| {
                if e.code == steve_node_api::Code::Unavailable {
                    CoreError::NoNodeAvailable(format!("本地设备节点离线：{}", e.safe_detail))
                } else {
                    CoreError::Msg(e.safe_detail)
                }
            })?;
        extra_kept.extend(resp.kept.iter().map(|k| Kept {
            item: k.item.clone(),
            reason: localize_kept_reason(&k.reason),
        }));
        let reclaimed = Reclaimed {
            skipped: false,
            free_before: resp.free_before,
            free_after: resp.free_after,
            freed: resp
                .freed
                .iter()
                .map(|f| Freed {
                    item: f.item.clone(),
                    caches: f.caches.clone(),
                    bytes: f.bytes,
                })
                .collect(),
            removed: resp.removed.clone(),
            kept: extra_kept,
        };
        Ok((reclaimed, resp))
    }

    /// Every worktree of an item, its size, caches and state, and what is in the folder besides,
    /// inspected by the colocated Node.
    pub fn disk_report(&self) -> Result<DiskReport> {
        let Some(port) = self.node_housekeeping() else {
            return Err(CoreError::NoNodeAvailable(
                "本地设备节点未连接，无法检查工作区磁盘占用".into(),
            ));
        };
        let root = self.config().worktrees.to_string_lossy().into_owned();
        let (mut entries, mut scratch) = {
            let t = self.tx();
            (t.workspace_entries()?, t.scratch_entries()?)
        };
        let req = DiskInspectRequest {
            worktrees_root: root.clone(),
            entries: entries
                .iter()
                .map(|e| steve_node_api::DiskInspectTarget {
                    item: e.item.clone(),
                    path: e.path.clone(),
                    repo: String::new(),
                    branch: String::new(),
                })
                .collect(),
            scratch: scratch
                .iter()
                .map(|e| steve_node_api::DiskInspectTarget {
                    item: e.item.clone(),
                    path: e.path.clone(),
                    repo: String::new(),
                    branch: String::new(),
                })
                .collect(),
        };
        let resp = crate::node_git::block_on(port.disk_inspect(req))
            .ok_or_else(|| CoreError::Msg("workspace disk inspect runtime unavailable".into()))?
            .map_err(|e| {
                if e.code == steve_node_api::Code::Unavailable {
                    CoreError::NoNodeAvailable(format!("本地设备节点离线：{}", e.safe_detail))
                } else {
                    CoreError::Msg(e.safe_detail)
                }
            })?;
        self.record_free(resp.free);
        let apply = |list: &mut [Entry], measures: Vec<steve_node_api::DiskEntryMeasure>| {
            let mut by_item: HashMap<String, steve_node_api::DiskEntryMeasure> =
                measures.into_iter().map(|m| (m.item.clone(), m)).collect();
            for e in list {
                if let Some(m) = by_item.remove(&e.item) {
                    e.exists = m.exists;
                    e.bytes = m.bytes;
                    e.cache_bytes = m.cache_bytes;
                    e.caches = m.caches;
                    e.in_use = m.in_use;
                }
            }
        };
        apply(&mut entries, resp.entries);
        apply(&mut scratch, resp.scratch);
        let pol = self.config().disk;
        Ok(DiskReport {
            root,
            free: resp.free,
            total: resp.total,
            pressure: resp.free.map(|f| pol.pressure(f)),
            policy: pol,
            git: resp.git,
            trash: resp.trash,
            entries,
            scratch,
            orphans: resp
                .orphans
                .into_iter()
                .map(|o| Orphan {
                    path: o.path,
                    bytes: o.bytes,
                })
                .collect(),
            measured_at: now(),
        })
    }

    /// At boot, then every ten minutes: a reclaim pass; when it leaves the disk below the floor, the
    /// workspaces hear of it once (again only after it was above `low` in between).
    pub fn watch_workspaces(self) -> steve_runtime::Task {
        let clock = self
            .modules()
            .map(|m| m.clock.clone())
            .unwrap_or_else(|| std::sync::Arc::new(steve_runtime::SystemClock));
        let warned = std::sync::Arc::new(std::sync::Mutex::new(Pressure::Ok));
        steve_runtime::every(
            "core.workspaces",
            clock,
            CHECK_EVERY,
            steve_runtime::FirstRun::Immediate,
            move || {
                let this = self.clone();
                let warned = warned.clone();
                async move {
                    let mut level = *warned.lock().unwrap_or_else(|e| e.into_inner());
                    let c = this.clone();
                    let r = match tokio::task::spawn_blocking(move || c.reclaim_workspaces()).await
                    {
                        Ok(Ok(r)) => r,
                        Ok(Err(CoreError::NoNodeAvailable(why))) => {
                            tracing::debug!("reclaim skipped: {why}");
                            return;
                        }
                        Ok(Err(e)) => {
                            tracing::error!("reclaim {e}");
                            return;
                        }
                        Err(e) => {
                            tracing::error!("reclaim task {e}");
                            return;
                        }
                    };
                    if r.freed_bytes() > 0 {
                        tracing::info!("reclaimed {}", human(r.freed_bytes()));
                    }
                    let pol = this.config().disk;
                    match r.free_after.map(|f| pol.pressure(f)) {
                        Some(Pressure::Floor) if level != Pressure::Floor => {
                            level = Pressure::Floor;
                            if let Err(e) = this.warn_floor(r.free_after.unwrap_or(0)) {
                                tracing::error!("disk notice {e}");
                            }
                        }
                        Some(Pressure::Low) if level == Pressure::Ok => {
                            level = Pressure::Low;
                            if let Err(e) = this.warn_low(r.free_after.unwrap_or(0)) {
                                tracing::error!("disk notice {e}");
                            }
                        }
                        Some(Pressure::Ok) => level = Pressure::Ok,
                        _ => {}
                    }
                    *warned.lock().unwrap_or_else(|e| e.into_inner()) = level;
                }
            },
        )
    }

    /// At boot, then every ten minutes: a reclaim pass; runs forever.
    pub async fn run_workspace_watch(self) {
        self.watch_workspaces().await;
    }

    fn warn_low(&self, free: u64) -> Result<()> {
        let t = self.tx();
        let text = steve_i18n::stored(|| {
            steve_i18n::t!(
                "core.notice.disk_low",
                free = human(free),
                low = human(self.config().disk.low),
                floor = human(self.config().disk.floor)
            )
        });
        for ws in t.db().workspace_ids()? {
            steve_store::notify::notify(t.db().store(), &ws, "disk", "info", &text, None)?;
        }
        Ok(())
    }

    fn warn_floor(&self, free: u64) -> Result<()> {
        let t = self.tx();
        let text = steve_i18n::stored(|| {
            steve_i18n::t!(
                "core.notice.disk_floor",
                free = human(free),
                floor = human(self.config().disk.floor)
            )
        });
        for ws in t.db().workspace_ids()? {
            steve_store::notify::notify(t.db().store(), &ws, "disk", "warn", &text, None)?;
        }
        Ok(())
    }
}

impl Tx<'_> {
    fn under_root(&self, cwd: &str) -> bool {
        self.under_worktrees(cwd)
    }

    /// Whether `cwd` is an item's own scratch directory (`STEVE_HOME/scratch/<id>`).
    fn own_scratch(&self, item: &str, cwd: &str) -> bool {
        Path::new(cwd) == self.cfg().scratch.join(item)
    }

    fn build_reclaim_request(&self) -> Result<(DiskReclaimRequest, Vec<Kept>)> {
        let pol = self.cfg().disk;
        let at = now();
        let mut git_projects = Vec::new();
        let mut closed_worktrees = Vec::new();
        let mut idle_worktrees: Vec<(i64, ReclaimCacheTarget)> = Vec::new();
        let mut ended_scratch: Vec<(i64, ReclaimCacheTarget)> = Vec::new();
        let mut extra_kept = Vec::new();

        for ws in self.db().workspace_ids()? {
            for p in self.db().projects(&ws)? {
                if p.kind == "git" && p.worktree {
                    git_projects.push(p.path);
                }
            }
            for i in self.db().items_all(&ws)? {
                let sessions = self.db().sessions_of_item(&i.id)?;
                let busy = sessions.iter().any(|s| self.busy(&s.id));
                if busy {
                    continue;
                }
                if let Some(cwd) = i.cwd.as_deref().filter(|c| self.under_root(c)) {
                    if i.is_closed() {
                        let mut releasing = false;
                        for s in &sessions {
                            if self.rt().release(&s.id)? != crate::Released::Gone {
                                releasing = true;
                            }
                        }
                        if releasing {
                            extra_kept.push(Kept {
                                item: i.id.clone(),
                                reason: "会话正在释放，稍后清理工作区".into(),
                            });
                            continue;
                        }
                        let repo = self
                            .db()
                            .project(i.project_id.as_deref())?
                            .map(|p| p.path)
                            .unwrap_or_default();
                        let branch = i.branch.clone().unwrap_or_default();
                        closed_worktrees.push(ReclaimClosedTarget {
                            item: i.id.clone(),
                            path: cwd.to_string(),
                            repo,
                            branch,
                            keep_unmerged: false,
                        });
                    } else if at - i.updated >= pol.idle_ms {
                        idle_worktrees.push((
                            i.updated,
                            ReclaimCacheTarget {
                                item: i.id.clone(),
                                path: cwd.to_string(),
                            },
                        ));
                    }
                } else if let Some(cwd) = i.cwd.as_deref().filter(|c| self.own_scratch(&i.id, c))
                    && i.is_closed()
                    && self.db().project(i.project_id.as_deref())?.is_none()
                {
                    ended_scratch.push((
                        i.updated,
                        ReclaimCacheTarget {
                            item: i.id.clone(),
                            path: cwd.to_string(),
                        },
                    ));
                }
            }
        }
        idle_worktrees.sort_by_key(|(u, _)| *u);
        ended_scratch.sort_by_key(|(u, _)| *u);
        Ok((
            DiskReclaimRequest {
                worktrees_root: self.cfg().worktrees.to_string_lossy().into_owned(),
                git_projects,
                closed_worktrees,
                idle_worktrees: idle_worktrees.into_iter().map(|(_, t)| t).collect(),
                ended_scratch: ended_scratch.into_iter().map(|(_, t)| t).collect(),
                low_bytes: pol.low,
            },
            extra_kept,
        ))
    }

    fn scratch_entries(&self) -> Result<Vec<Entry>> {
        let mut out = vec![];
        for ws in self.db().workspace_ids()? {
            for i in self.db().items_all(&ws)? {
                let Some(cwd) = i.cwd.as_deref().filter(|c| self.own_scratch(&i.id, c)) else {
                    continue;
                };
                let busy = self
                    .db()
                    .sessions_of_item(&i.id)?
                    .iter()
                    .any(|s| self.busy(&s.id));
                out.push(Entry {
                    item: i.id.clone(),
                    workspace: ws.clone(),
                    title: i.title.clone(),
                    phase: i.phase.as_str().into(),
                    archived: i.archived,
                    branch: None,
                    path: cwd.into(),
                    exists: false,
                    bytes: 0,
                    cache_bytes: 0,
                    caches: vec![],
                    busy,
                    in_use: None,
                    updated: i.updated,
                });
            }
        }
        Ok(out)
    }

    fn workspace_entries(&self) -> Result<Vec<Entry>> {
        let mut out = vec![];
        for ws in self.db().workspace_ids()? {
            for i in self.db().items_all(&ws)? {
                let Some(cwd) = i.cwd.as_deref().filter(|c| self.under_root(c)) else {
                    continue;
                };
                let busy = self
                    .db()
                    .sessions_of_item(&i.id)?
                    .iter()
                    .any(|s| self.busy(&s.id));
                out.push(Entry {
                    item: i.id.clone(),
                    workspace: ws.clone(),
                    title: i.title.clone(),
                    phase: i.phase.as_str().into(),
                    archived: i.archived,
                    branch: i.branch.clone(),
                    path: cwd.into(),
                    exists: false,
                    bytes: 0,
                    cache_bytes: 0,
                    caches: vec![],
                    busy,
                    in_use: None,
                    updated: i.updated,
                });
            }
        }
        Ok(out)
    }

    /// One timeline note per item for the space its caches freed.
    pub(crate) fn note_freed(&self, freed: &[steve_node_api::DiskFreed]) -> Result<()> {
        for f in freed.iter().filter(|f| f.bytes > 0) {
            if self.db().item(&f.item)?.is_none() {
                continue;
            }
            let what = if f.caches.is_empty() {
                "构建缓存".to_string()
            } else {
                f.caches.join("、")
            };
            self.db().timeline_add(
                &f.item,
                "steve",
                "sys",
                &format!(
                    "回收了可重建的缓存 {what}，释放 {}；源码和提交都保留，需要时重新构建",
                    human(f.bytes)
                ),
                None,
            )?;
        }
        Ok(())
    }

    fn note_reclaim_outcomes(&self, resp: &DiskReclaimResponse) -> Result<()> {
        for (item, branch) in &resp.deleted_branches {
            let Some(i) = self.db().item(item)? else {
                continue;
            };
            let text = steve_i18n::stored(|| match i.cwd.as_deref().filter(|c| !c.is_empty()) {
                Some(cwd) => steve_i18n::t!(
                    "core.worktree.removed_merged",
                    cwd = cwd,
                    branch = branch.as_str()
                ),
                None => steve_i18n::t!("core.worktree.branch_deleted", branch = branch.as_str()),
            });
            self.note_cleanup_once(item, &text)?;
        }
        for (item, cwd, branch, head) in &resp.kept_branches {
            if self.db().item(item)?.is_none() {
                continue;
            }
            let text = steve_i18n::stored(|| {
                if head.is_empty() {
                    steve_i18n::t!(
                        "core.worktree.removed_unmerged_no_sha",
                        cwd = cwd.as_str(),
                        branch = branch.as_str()
                    )
                } else {
                    steve_i18n::t!(
                        "core.worktree.removed_unmerged",
                        cwd = cwd.as_str(),
                        branch = branch.as_str(),
                        head = head.as_str()
                    )
                }
            });
            self.note_cleanup_once(item, &text)?;
        }
        for (item, branch, err) in &resp.branch_delete_failed {
            let Some(i) = self.db().item(item)? else {
                continue;
            };
            let cwd = i.cwd.as_deref().unwrap_or("");
            let text = steve_i18n::stored(|| {
                steve_i18n::t!(
                    "core.worktree.removed_branch_failed",
                    cwd = cwd,
                    branch = branch.as_str(),
                    error = err.as_str()
                )
            });
            self.note_cleanup_once(item, &text)?;
        }
        for (item, branch) in &resp.absent_deleted_branches {
            if self.db().item(item)?.is_none() {
                continue;
            }
            let text = steve_i18n::stored(|| {
                steve_i18n::t!("core.worktree.branch_deleted", branch = branch.as_str())
            });
            self.note_cleanup_once(item, &text)?;
        }
        for (item, branch, err) in &resp.absent_branch_delete_failed {
            if self.db().item(item)?.is_none() {
                continue;
            }
            let text = steve_i18n::stored(|| {
                steve_i18n::t!(
                    "core.worktree.branch_delete_failed",
                    branch = branch.as_str(),
                    error = err.as_str()
                )
            });
            self.note_cleanup_once(item, &text)?;
        }
        for k in &resp.kept {
            let Some(i) = self.db().item(&k.item)? else {
                continue;
            };
            let cwd = i.cwd.as_deref().unwrap_or("");
            let text = if k.reason == "in_use" {
                steve_i18n::stored(|| steve_i18n::t!("core.worktree.cleanup_in_use", cwd = cwd))
            } else if let Some(detail) = k.detail.as_deref() {
                steve_i18n::stored(|| {
                    steve_i18n::t!("core.worktree.cleanup_failed", cwd = cwd, error = detail)
                })
            } else {
                continue;
            };
            self.note_cleanup_once(&k.item, &text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn configured_values_are_strict_and_each_is_read_once() {
        let reads = std::cell::RefCell::new(Vec::new());
        let policy = DiskPolicy::from_vars(|var| {
            reads.borrow_mut().push(var.to_owned());
            match var {
                "STEVE_DISK_FLOOR_GB" => Some("1.5".into()),
                "STEVE_DISK_LOW_GB" => Some("2".into()),
                "STEVE_WORKTREE_IDLE_HOURS" => Some("0.5".into()),
                _ => None,
            }
        })
        .unwrap();
        assert_eq!(
            *reads.borrow(),
            [
                "STEVE_DISK_FLOOR_GB",
                "STEVE_DISK_LOW_GB",
                "STEVE_WORKTREE_IDLE_HOURS"
            ]
        );
        assert_eq!(
            policy,
            DiskPolicy {
                floor: GB * 3 / 2,
                low: GB * 2,
                idle_ms: 1_800_000
            }
        );
        for var in [
            "STEVE_DISK_FLOOR_GB",
            "STEVE_DISK_LOW_GB",
            "STEVE_WORKTREE_IDLE_HOURS",
        ] {
            for invalid in ["", "oops", "NaN", "inf", "-1", "1e99"] {
                let error = DiskPolicy::from_vars(|name| (name == var).then(|| invalid.into()))
                    .unwrap_err();
                assert_eq!(error.var, var);
            }
        }
    }

    #[test]
    fn default_low_follows_floor_but_an_explicit_lower_value_is_rejected() {
        assert_eq!(
            DiskPolicy::from_vars(|_| None).unwrap(),
            DiskPolicy::default()
        );
        let policy =
            DiskPolicy::from_vars(|var| (var == "STEVE_DISK_FLOOR_GB").then(|| "50".into()))
                .unwrap();
        assert_eq!((policy.floor, policy.low), (50 * GB, 50 * GB));
        let error = DiskPolicy::from_vars(|var| match var {
            "STEVE_DISK_FLOOR_GB" => Some("50".into()),
            "STEVE_DISK_LOW_GB" => Some("40".into()),
            _ => None,
        })
        .unwrap_err();
        assert_eq!(error.var, "STEVE_DISK_LOW_GB");
    }
}
