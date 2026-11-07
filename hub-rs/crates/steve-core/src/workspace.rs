//! Task workspaces (ST-45): how much disk the items' worktrees take, giving space back, and the floor below
//! which no new worktree is made.
//!
//! A worktree is the item's sources (small; the git objects are shared with the project) plus rebuildable
//! caches — Rust `target/`, `node_modules` — which are almost all of its size (measured on Steve's own repo:
//! 16 MB of sources next to up to 17 GB of `target/`). So:
//! - a closed item gives its caches back at once, and its worktree once that loses nothing: clean, and its
//!   work landed in main or the item is archived (the branch always stays; [`crate::git::tidy_worktree`]);
//! - when free space falls below [`DiskPolicy::low`], idle items' caches go too (no session busy, no process
//!   inside, untouched for [`DiskPolicy::idle_ms`]), oldest first, until there is enough again;
//! - below [`DiskPolicy::floor`] no new worktree is made ([`Core::disk_floor`]);
//! - an item without a project works in a scratch directory (`STEVE_HOME/scratch/<id>`), which has no git
//!   to ask what is rebuildable: once the item ended, and only under pressure, its directories with a known
//!   cache name or a `CACHEDIR.TAG` go ([`scratch_cache_dirs`]); everything else in it stays.
//!
//! Nothing is touched while a session of the item is busy or a process runs inside the worktree. Deleting
//! tens of GB takes a while, so it never happens under the core's lock: caches are renamed into
//! `worktrees/.trash` (atomic, same volume) under the lock and deleted outside it. A crash leaves them in the
//! trash and the next pass deletes them, so a pass can be interrupted anywhere and the next one finishes it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::Result;
use crate::git;
use crate::repo::ItemExt;
use crate::util::now;
use crate::{Core, Tx};

pub const GB: u64 = 1 << 30;
/// How often the watcher looks at the disk and gives back what it can.
const CHECK_EVERY: Duration = Duration::from_secs(600);
/// Where removed caches wait to be deleted, inside the worktrees directory (same volume: a rename).
const TRASH: &str = ".trash";
/// Dependency and tool caches by name; anything else counts as a cache only with a `CACHEDIR.TAG`
/// (Cargo writes one into every `target/`).
const CACHE_NAMES: &[&str] = &[
    "node_modules",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    ".svelte-kit",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".gradle",
];
/// How deep caches are looked for (`hub-rs/target`, `web/node_modules`).
const CACHE_DEPTH: usize = 4;
/// How long listing the processes may take before Steve treats them as unknown.
const PS_DEADLINE: Duration = Duration::from_secs(10);

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
    /// One cold test build of Steve writes 12–16 GB, a JS project's `node_modules` under 1 GB: below the
    /// floor even a small project risks filling the disk mid-build; `low` leaves room for a few Rust builds.
    fn default() -> Self {
        DiskPolicy {
            floor: 10 * GB,
            low: 40 * GB,
            idle_ms: 24 * 3_600_000,
        }
    }
}

impl DiskPolicy {
    pub fn from_env() -> DiskPolicy {
        DiskPolicy::from_vars(|k| std::env::var(k).ok())
    }

    /// From a variable map (the Hub's configuration); unset or invalid values keep the defaults.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> DiskPolicy {
        let d = DiskPolicy::default();
        let num = |k: &str| {
            get(k)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0)
        };
        let floor = num("STEVE_DISK_FLOOR_GB").map_or(d.floor, |g| (g * GB as f64) as u64);
        let low = num("STEVE_DISK_LOW_GB").map_or(d.low, |g| (g * GB as f64) as u64);
        DiskPolicy {
            floor,
            low: low.max(floor),
            idle_ms: num("STEVE_WORKTREE_IDLE_HOURS")
                .map_or(d.idle_ms, |h| (h * 3_600_000.0) as i64),
        }
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

/// `(free, total)` bytes of the volume `path` is on, as an unprivileged user may use it.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let unit = s.f_frsize as u64;
    Some((s.f_bavail as u64 * unit, s.f_blocks as u64 * unit))
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

/// The rebuildable caches in worktree `wt`: directories that git ignores and tracks nothing in, and that are
/// a known dependency or tool cache or marked as a cache (`CACHEDIR.TAG`). Neither `.git` nor the caches
/// themselves are looked into; symlinks are not followed.
pub fn cache_dirs(wt: &Path) -> Vec<PathBuf> {
    let mut found = marked_caches(wt);
    found.retain(|p| rebuildable(wt, p));
    found
}

/// The caches in an ended item's scratch directory `dir`: only directories with a known cache name or a
/// `CACHEDIR.TAG` (there is no git to ask what else is rebuildable). Symlinks are not followed.
pub fn scratch_cache_dirs(dir: &Path) -> Vec<PathBuf> {
    marked_caches(dir)
}

/// Directories under `wt` (to [`CACHE_DEPTH`]) with a known cache name or a `CACHEDIR.TAG`, sorted;
/// neither `.git` nor the caches themselves are looked into.
fn marked_caches(wt: &Path) -> Vec<PathBuf> {
    let mut found = vec![];
    let mut todo = vec![(wt.to_path_buf(), 0)];
    while let Some((dir, depth)) = todo.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) || e.file_name() == ".git" {
                continue;
            }
            let p = e.path();
            if CACHE_NAMES.iter().any(|n| e.file_name() == *n) || p.join("CACHEDIR.TAG").is_file() {
                found.push(p);
            } else if depth + 1 < CACHE_DEPTH {
                todo.push((p, depth + 1));
            }
        }
    }
    found.sort();
    found
}

/// Ignored by git and nothing in it tracked: deleting it loses nothing a commit holds.
fn rebuildable(wt: &Path, dir: &Path) -> bool {
    let Ok(rel) = dir.strip_prefix(wt) else {
        return false;
    };
    let rel = rel.to_string_lossy();
    git::git_test(wt, &["check-ignore", "-q", &format!("{rel}/")]).unwrap_or(false)
        && git::git(wt, &["ls-files", "--", &rel]).is_ok_and(|o| o.is_empty())
}

/// What `dir` takes on disk: its allocated blocks. A file sharing blocks with another (an APFS clone, a hard
/// link) counts in full, so this can overstate what deleting it frees.
pub fn dir_bytes(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut sum = fs::symlink_metadata(dir).map_or(0, |m| m.blocks() * 512);
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            /* `DirEntry::metadata` does not follow symlinks */
            let Ok(m) = e.metadata() else {
                continue;
            };
            sum += m.blocks() * 512;
            if m.is_dir() {
                todo.push(e.path());
            }
        }
    }
    sum
}

/// The working directories of this user's running processes; `None` when they can't be listed.
pub fn process_dirs() -> Option<Vec<PathBuf>> {
    #[cfg(target_os = "linux")]
    {
        let mut out = vec![];
        for e in fs::read_dir("/proc").ok()?.flatten() {
            if e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
                && let Ok(p) = fs::read_link(e.path().join("cwd"))
            {
                out.push(p);
            }
        }
        Some(out)
    }
    #[cfg(not(target_os = "linux"))]
    {
        /* `lsof -d cwd` lists every process's working directory in one call (about a second) */
        let mut child = Command::new("lsof")
            .args(["-nP", "-w", "-d", "cwd", "-Fn"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut stdout, &mut s).map(|_| s)
        });
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < PS_DEADLINE => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let text = reader.join().ok()?.ok()?;
        let dirs: Vec<PathBuf> = text
            .lines()
            .filter_map(|l| l.strip_prefix('n'))
            .map(PathBuf::from)
            .collect();
        /* lsof exits 1 when some process could not be read; an empty list means it saw nothing */
        (!dirs.is_empty()).then_some(dirs)
    }
}

/// Whether a process runs inside `dir`. The process list holds resolved paths (`/private/tmp`, not `/tmp`),
/// so `dir` is resolved too.
pub fn in_use(dir: &Path, procs: &[PathBuf]) -> bool {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    procs.iter().any(|p| p.starts_with(&dir))
}

pub fn trash_dir(worktrees: &Path) -> PathBuf {
    worktrees.join(TRASH)
}

/// Moves `dirs` (caches inside worktree `wt` of `item`) into one trash folder `<item>@<ms>`. A cache that
/// can't be moved (another volume, gone meanwhile) stays where it is. Returns the paths moved, relative to
/// `wt`.
pub(crate) fn to_trash(trash: &Path, item: &str, wt: &Path, dirs: &[PathBuf]) -> Vec<String> {
    if dirs.is_empty() {
        return vec![];
    }
    let folder = trash.join(format!("{item}@{}", now()));
    if let Err(e) = fs::create_dir_all(&folder) {
        tracing::warn!("trash {}: {e}", folder.display());
        return vec![];
    }
    let mut moved = vec![];
    for (n, d) in dirs.iter().enumerate() {
        let rel = d
            .strip_prefix(wt)
            .unwrap_or(d)
            .to_string_lossy()
            .to_string();
        let to = folder.join(format!("{n}-{}", rel.replace('/', "__")));
        match fs::rename(d, &to) {
            Ok(()) => moved.push(rel),
            Err(e) => tracing::warn!("move {} to trash: {e}", d.display()),
        }
    }
    if moved.is_empty() {
        let _ = fs::remove_dir(&folder);
    }
    moved
}

/// What one trash folder held once deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Freed {
    pub item: String,
    /// The caches, relative to the worktree (`hub-rs/target`).
    pub caches: Vec<String>,
    pub bytes: u64,
}

/// Deletes everything in the trash, folder by folder; a folder that can't be deleted entirely stays for the
/// next pass.
pub fn purge(trash: &Path) -> Vec<Freed> {
    let mut out: Vec<Freed> = vec![];
    let mut failed: Vec<PathBuf> = vec![];
    /* folders moved in while this runs are taken too */
    loop {
        let Ok(rd) = fs::read_dir(trash) else {
            break;
        };
        let next: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| !failed.contains(p))
            .collect();
        if next.is_empty() {
            break;
        }
        for folder in next {
            let name = folder
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let item = name
                .split_once('@')
                .map_or(name.as_str(), |(i, _)| i)
                .to_string();
            let caches = fs::read_dir(&folder)
                .map(|rd| {
                    let mut c: Vec<String> = rd
                        .flatten()
                        .map(|e| {
                            let n = e.file_name().to_string_lossy().to_string();
                            n.split_once('-')
                                .map_or(n.clone(), |(_, r)| r.to_string())
                                .replace("__", "/")
                        })
                        .collect();
                    c.sort();
                    c
                })
                .unwrap_or_default();
            let bytes = dir_bytes(&folder);
            let gone = if folder.is_dir() {
                fs::remove_dir_all(&folder)
            } else {
                fs::remove_file(&folder)
            };
            if let Err(e) = gone {
                tracing::warn!("purge {}: {e}", folder.display());
                failed.push(folder);
                continue;
            }
            match out.iter_mut().find(|f| f.item == item) {
                Some(f) => {
                    f.bytes += bytes;
                    f.caches.extend(caches);
                }
                None => out.push(Freed {
                    item,
                    caches,
                    bytes,
                }),
            }
        }
    }
    out
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
    pub status: String,
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

/// An item with a worktree under Steve's worktrees folder, as the pass sees it under the lock.
struct Candidate {
    item: String,
    wt: PathBuf,
    finished: bool,
    updated: i64,
    /// The project's repository and the item's branch, when it has both.
    repo: Option<(PathBuf, String)>,
}

impl Core {
    /// The floor check before a new worktree is made: below [`DiskPolicy::floor`] it is refused, and a pass
    /// starts giving space back.
    pub(crate) fn disk_floor(&self) -> Result<()> {
        let pol = self.config().disk;
        let Some((free, _)) = space(&self.config().worktrees) else {
            return Ok(());
        };
        if free >= pol.floor {
            return Ok(());
        }
        Err(crate::error::msg(self.disk_low(free)))
    }

    /// Why no new worktree is made with `free` bytes left (below the floor), and a pass starts
    /// giving space back.
    pub(crate) fn disk_low(&self, free: u64) -> String {
        let c = self.clone();
        crate::detach(move || {
            if let Err(e) = c.reclaim_workspaces() {
                tracing::error!("reclaim {e}");
            }
        });
        steve_i18n::t!(
            "core.workspace.disk_low",
            free = human(free),
            floor = human(self.config().disk.floor)
        )
    }

    /// The free bytes where worktrees go, if they can be told.
    pub(crate) fn worktrees_free(&self) -> Option<u64> {
        space(&self.config().worktrees).map(|s| s.0)
    }

    /// Deletes what is in the trash on a thread of its own, unless a pass is already running (it takes
    /// what comes in meanwhile).
    pub(crate) fn purge_soon(&self) {
        let c = self.clone();
        crate::detach(move || {
            if c.0.reclaiming.swap(true, Ordering::SeqCst) {
                return;
            }
            let freed = purge(&trash_dir(&c.config().worktrees));
            c.0.reclaiming.store(false, Ordering::SeqCst);
            if let Err(e) = c.tx().note_freed(&freed) {
                tracing::error!("reclaim note {e}");
            }
        });
    }

    /// One pass: deletes what an earlier pass left in the trash, gives back closed items' caches and their
    /// worktrees where that is safe, and under pressure idle items' caches, oldest first, until free space is
    /// above [`DiskPolicy::low`]. One pass at a time; a second caller gets `skipped`.
    pub fn reclaim_workspaces(&self) -> Result<Reclaimed> {
        if self.0.reclaiming.swap(true, Ordering::SeqCst) {
            return Ok(Reclaimed {
                skipped: true,
                ..Default::default()
            });
        }
        let r = self.reclaim_pass();
        self.0.reclaiming.store(false, Ordering::SeqCst);
        let r = r?;
        self.tx().note_freed(&r.freed)?;
        Ok(r)
    }

    fn reclaim_pass(&self) -> Result<Reclaimed> {
        let root = self.config().worktrees.clone();
        let trash = trash_dir(&root);
        let pol = self.config().disk;
        let free = || space(&root).map(|s| s.0);
        let mut r = Reclaimed {
            free_before: free(),
            ..Default::default()
        };
        r.freed = purge(&trash);
        let procs = process_dirs();
        let candidates = self.tx().workspace_candidates()?;
        self.tx().prune_worktrees()?;

        /* closed items: their caches, and their worktree where nothing is lost */
        for c in candidates.iter().filter(|c| c.finished) {
            if procs.as_deref().is_some_and(|p| in_use(&c.wt, p)) {
                r.kept.push(Kept {
                    item: c.item.clone(),
                    reason: "有进程还在工作区里运行".into(),
                });
                continue;
            }
            /* the slow part of the tidy's decision, outside the lock (it is remembered) */
            if let Some((repo, branch)) = &c.repo {
                git::landed(repo, branch, "main");
            }
            let mut t = self.tx();
            t.tidy_branch_now(&c.item)?;
            if Path::new(&c.wt).exists() {
                r.kept.push(Kept {
                    item: c.item.clone(),
                    reason: t.why_kept(&c.item, &c.wt),
                });
            } else {
                r.removed.push(c.item.clone());
            }
        }
        r.freed.extend(purge(&trash));

        /* under pressure: idle open items' caches, oldest first, until there is enough again; then the
         * scratch directories of ended items without a project, oldest first */
        let at = now();
        let mut idle: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| !c.finished && at - c.updated >= pol.idle_ms)
            .collect();
        idle.sort_by_key(|c| c.updated);
        for c in idle {
            if free().is_none_or(|f| pol.pressure(f) == Pressure::Ok) {
                break;
            }
            /* without the process list, an open item's worktree is never touched */
            if procs.as_deref().is_none_or(|p| in_use(&c.wt, p)) {
                continue;
            }
            if self.tx().shed_caches(&c.item, &c.wt)? {
                r.freed.extend(purge(&trash));
            }
        }
        if free().is_some_and(|f| pol.pressure(f) != Pressure::Ok) {
            let mut ended = self.tx().scratch_candidates()?;
            ended.sort_by_key(|c| c.updated);
            for c in ended {
                if free().is_none_or(|f| pol.pressure(f) == Pressure::Ok) {
                    break;
                }
                if procs.as_deref().is_none_or(|p| in_use(&c.wt, p)) {
                    continue;
                }
                if self.tx().shed_scratch_caches(&c.item, &c.wt)? {
                    r.freed.extend(purge(&trash));
                }
            }
        }
        r.free_after = free();
        Ok(r)
    }

    /// Every worktree of an item, its size, caches and state, and what is in the folder besides. Walks the
    /// worktrees (seconds for a hundred GB); never under the lock.
    pub fn disk_report(&self) -> Result<DiskReport> {
        let root = self.config().worktrees.clone();
        let (mut entries, mut scratch) = {
            let t = self.tx();
            (t.workspace_entries()?, t.scratch_entries()?)
        };
        let procs = process_dirs();
        for (e, in_scratch) in entries
            .iter_mut()
            .map(|e| (e, false))
            .chain(scratch.iter_mut().map(|e| (e, true)))
            .filter(|(e, _)| e.exists)
        {
            let wt = Path::new(&e.path);
            let caches = if in_scratch {
                scratch_cache_dirs(wt)
            } else {
                cache_dirs(wt)
            };
            e.cache_bytes = caches.iter().map(|c| dir_bytes(c)).sum();
            e.caches = caches
                .iter()
                .filter_map(|c| c.strip_prefix(wt).ok())
                .map(|c| c.to_string_lossy().to_string())
                .collect();
            e.bytes = dir_bytes(wt);
            e.in_use = procs.as_deref().map(|p| in_use(wt, p));
        }
        let known: Vec<PathBuf> = entries.iter().map(|e| PathBuf::from(&e.path)).collect();
        let mut orphans = vec![];
        if let Ok(rd) = fs::read_dir(&root) {
            for d in rd.flatten() {
                let p = d.path();
                if d.file_name() == TRASH || !p.is_dir() || known.contains(&p) {
                    continue;
                }
                orphans.push(Orphan {
                    path: p.to_string_lossy().into(),
                    bytes: dir_bytes(&p),
                });
            }
        }
        orphans.sort_by(|a, b| a.path.cmp(&b.path));
        let sp = space(&root);
        let pol = self.config().disk;
        Ok(DiskReport {
            root: root.to_string_lossy().into(),
            free: sp.map(|s| s.0),
            total: sp.map(|s| s.1),
            pressure: sp.map(|s| pol.pressure(s.0)),
            policy: pol,
            git: git::version().map(|(a, b)| format!("{a}.{b}")),
            trash: dir_bytes(&trash_dir(&root)),
            entries,
            scratch,
            orphans,
            measured_at: now(),
        })
    }

    /// At boot, then every ten minutes: a reclaim pass; when it leaves the disk below the floor, the
    /// workspaces hear of it once (again only after it was above `low` in between). Runs forever.
    pub async fn run_workspace_watch(self) {
        let mut tick = tokio::time::interval(CHECK_EVERY);
        let mut warned = false;
        loop {
            tick.tick().await;
            let c = self.clone();
            let r = match tokio::task::spawn_blocking(move || c.reclaim_workspaces()).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    tracing::error!("reclaim {e}");
                    continue;
                }
                Err(e) => {
                    tracing::error!("reclaim task {e}");
                    continue;
                }
            };
            if r.freed_bytes() > 0 {
                tracing::info!("reclaimed {}", human(r.freed_bytes()));
            }
            let pol = self.config().disk;
            match r.free_after.map(|f| pol.pressure(f)) {
                Some(Pressure::Floor) if !warned => {
                    warned = true;
                    if let Err(e) = self.warn_floor(r.free_after.unwrap_or(0)) {
                        tracing::error!("disk notice {e}");
                    }
                }
                Some(Pressure::Ok) => warned = false,
                _ => {}
            }
        }
    }

    fn warn_floor(&self, free: u64) -> Result<()> {
        let t = self.tx();
        let text = format!(
            "磁盘只剩 {}，低于开工下限 {}：Steve 已回收能回收的构建缓存，新事项暂时不会新建工作区。请清理磁盘，或在“设置 · 数据 · 工作区与磁盘”查看占用。",
            human(free),
            human(self.config().disk.floor)
        );
        for ws in t.db().workspace_ids()? {
            crate::notify::notify(t.db(), &ws, "disk", "warn", &text, None)?;
        }
        Ok(())
    }
}

impl Tx<'_> {
    fn under_root(&self, cwd: &str) -> bool {
        let prefix = format!("{}/", self.cfg().worktrees.to_string_lossy());
        cwd.starts_with(&prefix)
    }

    /// Whether `cwd` is an item's own scratch directory (`STEVE_HOME/scratch/<id>`).
    fn own_scratch(&self, item: &str, cwd: &str) -> bool {
        Path::new(cwd) == self.cfg().scratch.join(item)
    }

    /// Ended items without a project whose scratch directory exists and that no session works in now.
    fn scratch_candidates(&self) -> Result<Vec<Candidate>> {
        let mut out = vec![];
        for ws in self.db().workspace_ids()? {
            for i in self.db().items_all(&ws)? {
                let Some(cwd) = i.cwd.as_deref().filter(|c| self.own_scratch(&i.id, c)) else {
                    continue;
                };
                if !i.is_finished()
                    || self.db().project(i.project_id.as_deref())?.is_some()
                    || !Path::new(cwd).is_dir()
                    || self
                        .db()
                        .sessions_of_item(&i.id)?
                        .iter()
                        .any(|s| self.busy(&s.id))
                {
                    continue;
                }
                out.push(Candidate {
                    item: i.id.clone(),
                    wt: cwd.into(),
                    finished: true,
                    updated: i.updated,
                    repo: None,
                });
            }
        }
        Ok(out)
    }

    /// Items' scratch directories, as the disk report lists them.
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
                    status: i.status.clone(),
                    phase: i.phase.clone(),
                    archived: i.archived,
                    branch: None,
                    path: cwd.into(),
                    exists: Path::new(cwd).is_dir(),
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

    /// Moves an ended project-less item's scratch caches to the trash, checking under the lock that it
    /// is still ended and idle.
    fn shed_scratch_caches(&mut self, item: &str, dir: &Path) -> Result<bool> {
        let Some(i) = self.db().item(item)? else {
            return Ok(false);
        };
        if !i.is_finished()
            || self.db().project(i.project_id.as_deref())?.is_some()
            || self
                .db()
                .sessions_of_item(item)?
                .iter()
                .any(|s| self.busy(&s.id))
        {
            return Ok(false);
        }
        let moved = to_trash(
            &trash_dir(&self.cfg().worktrees),
            item,
            dir,
            &scratch_cache_dirs(dir),
        );
        Ok(!moved.is_empty())
    }

    /// Items whose worktree exists under Steve's worktrees folder and that no session works in now.
    fn workspace_candidates(&self) -> Result<Vec<Candidate>> {
        let mut out = vec![];
        for ws in self.db().workspace_ids()? {
            for i in self.db().items_all(&ws)? {
                let Some(cwd) = i.cwd.as_deref().filter(|c| self.under_root(c)) else {
                    continue;
                };
                if !Path::new(cwd).is_dir()
                    || self
                        .db()
                        .sessions_of_item(&i.id)?
                        .iter()
                        .any(|s| self.busy(&s.id))
                {
                    continue;
                }
                let repo = match (
                    self.db().project(i.project_id.as_deref())?,
                    i.branch.as_deref().filter(|b| !b.is_empty()),
                ) {
                    (Some(p), Some(b)) => Some((PathBuf::from(p.path), b.to_string())),
                    _ => None,
                };
                out.push(Candidate {
                    item: i.id.clone(),
                    wt: cwd.into(),
                    finished: i.is_finished(),
                    updated: i.updated,
                    repo,
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
                    status: i.status.clone(),
                    phase: i.phase.clone(),
                    archived: i.archived,
                    branch: i.branch.clone(),
                    path: cwd.into(),
                    exists: Path::new(cwd).is_dir(),
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

    /// `git worktree prune` in every git project with worktrees: forgets worktrees whose folder is gone (a
    /// removal cut short, a folder deleted by hand).
    fn prune_worktrees(&self) -> Result<()> {
        for ws in self.db().workspace_ids()? {
            for p in self.db().projects(&ws)? {
                if p.kind == "git"
                    && p.worktree
                    && Path::new(&p.path).is_dir()
                    && let Err(e) = git::git(Path::new(&p.path), &["worktree", "prune"])
                {
                    tracing::warn!("worktree prune {}: {e}", p.path);
                }
            }
        }
        Ok(())
    }

    /// Moves an idle open item's caches to the trash, checking under the lock that it is still idle.
    fn shed_caches(&mut self, item: &str, wt: &Path) -> Result<bool> {
        let Some(i) = self.db().item(item)? else {
            return Ok(false);
        };
        if i.is_finished()
            || now() - i.updated < self.cfg().disk.idle_ms
            || self
                .db()
                .sessions_of_item(item)?
                .iter()
                .any(|s| self.busy(&s.id))
        {
            return Ok(false);
        }
        let moved = to_trash(&trash_dir(&self.cfg().worktrees), item, wt, &cache_dirs(wt));
        Ok(!moved.is_empty())
    }

    /// Moves a closed item's caches to the trash (the caller already checked nothing runs in `wt`); the
    /// paths moved.
    pub(crate) fn shed_closed_caches(&mut self, item: &str, wt: &Path) -> Vec<String> {
        to_trash(&trash_dir(&self.cfg().worktrees), item, wt, &cache_dirs(wt))
    }

    /// Why a closed item's worktree stays.
    fn why_kept(&self, item: &str, wt: &Path) -> String {
        let dirty = git::git_read(wt, &["status", "--porcelain"]).map(|s| !s.is_empty());
        let archived = self
            .db()
            .item(item)
            .ok()
            .flatten()
            .is_some_and(|i| i.archived);
        match dirty {
            Ok(true) => "有未提交的改动，保留工作区".into(),
            Ok(false) if !archived => "分支还没合入主线，归档后回收工作区（分支保留）".into(),
            Ok(false) => "工作区未能删除，下次再试".into(),
            Err(_) => "看不出工作区的状态，保留".into(),
        }
    }

    /// One timeline note per item for the space its caches freed.
    pub(crate) fn note_freed(&self, freed: &[Freed]) -> Result<()> {
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
}
