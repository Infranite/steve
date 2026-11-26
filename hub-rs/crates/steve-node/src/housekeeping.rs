//! Workspace and scratch disk housekeeping on this Node.
//!
//! Measures worktrees, scratch directories, rebuildable caches, and orphans; prunes git worktree
//! metadata; reclaims closed worktrees and rebuildable caches via atomic rename into `.trash`
//! followed by background-safe deletion. The Hub supplies item state and disk thresholds; all
//! filesystem inspection, process detection, and git worktree operations stay on the Node.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use steve_node_api::*;
use steve_toolchains::ITEM_BUILD_DIR;

use crate::space;
use crate::workspace::common_git_dir;

/// Where removed caches wait to be deleted, inside the worktrees directory (same volume: a rename).
const TRASH: &str = ".trash";
/// How deep caches are looked for (`hub-rs/target`, `web/node_modules`).
const CACHE_DEPTH: usize = 4;
/// How long listing processes may take before treating them as unknown.
#[cfg(not(target_os = "linux"))]
const PS_DEADLINE: Duration = Duration::from_secs(10);
/// How long one read-only git command may take.
const GIT_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long waiting for the common `.git` worktree flock may take.
const WORKTREE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum cached entries in the `landed` memo table.
const LANDED_MEMO_CAP: usize = 4096;

/// Reason codes reported in [`DiskKept::reason`] (localized by the Hub/Core edge).
pub const REASON_IN_USE: &str = "in_use";
pub const REASON_DIRTY: &str = "dirty";
pub const REASON_UNMERGED: &str = "unmerged";
pub const REASON_REMOVE_FAILED: &str = "remove_failed";
pub const REASON_UNKNOWN_STATE: &str = "unknown_state";

fn cache_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        steve_toolchains::Toolchains::builtin()
            .disposable_names()
            .into_iter()
            .map(String::from)
            .collect()
    })
}

fn is_disposable_dir(dir: &Path, workspace_root: &Path) -> bool {
    fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        && (dir
            .parent()
            .is_some_and(|p| p == workspace_root.join(ITEM_BUILD_DIR))
            || cache_names()
                .iter()
                .any(|n| dir.file_name().is_some_and(|name| name == n.as_str()))
            || dir.join("CACHEDIR.TAG").is_file())
}

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
            if is_disposable_dir(&p, wt) {
                found.push(p);
            } else if depth + 1 < CACHE_DEPTH {
                todo.push((p, depth + 1));
            }
        }
    }
    found.sort();
    found
}

fn rebuildable(wt: &Path, dir: &Path) -> bool {
    let Ok(rel) = dir.strip_prefix(wt) else {
        return false;
    };
    let rel = rel.to_string_lossy();
    git_test(wt, &["check-ignore", "-q", &format!("{rel}/")]).unwrap_or(false)
        && git_read(wt, &["ls-files", "--", &rel]).is_ok_and(|o| o.is_empty())
}

pub fn cache_dirs(wt: &Path) -> Vec<PathBuf> {
    let mut found = marked_caches(wt);
    found.retain(|p| rebuildable(wt, p));
    found
}

pub fn scratch_cache_dirs(dir: &Path) -> Vec<PathBuf> {
    marked_caches(dir)
}

pub fn space(dir: &Path) -> Option<(u64, u64)> {
    space::measure(dir).map(|s| (s.free, s.total))
}

fn dir_bytes(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut sum = fs::symlink_metadata(dir).map_or(0, |m| m.blocks() * 512);
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
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
                    std::thread::sleep(Duration::from_millis(50));
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
        (!dirs.is_empty()).then_some(dirs)
    }
}

pub fn in_use(dir: &Path, procs: &[PathBuf]) -> bool {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    procs.iter().any(|p| p.starts_with(&dir))
}

pub fn trash_dir(worktrees: &Path) -> PathBuf {
    worktrees.join(TRASH)
}

pub fn to_trash(trash: &Path, item: &str, wt: &Path, dirs: &[PathBuf]) -> Vec<String> {
    if dirs.is_empty() {
        return vec![];
    }
    let folder = trash.join(format!("{item}@{}", crate::now()));
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

fn merge_freed(out: &mut Vec<DiskFreed>, added: Vec<DiskFreed>) {
    for f in added {
        match out.iter_mut().find(|x| x.item == f.item) {
            Some(existing) => {
                existing.bytes = existing.bytes.saturating_add(f.bytes);
                existing.caches.extend(f.caches);
            }
            None => out.push(f),
        }
    }
}

pub fn purge(trash: &Path) -> Vec<DiskFreed> {
    let mut out: Vec<DiskFreed> = vec![];
    let mut failed: Vec<PathBuf> = vec![];
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
                    f.bytes = f.bytes.saturating_add(bytes);
                    f.caches.extend(caches);
                }
                None => out.push(DiskFreed {
                    item,
                    caches,
                    bytes,
                }),
            }
        }
    }
    out
}

// ── local git helpers for housekeeping ──

fn git_cmd() -> Command {
    let mut c = Command::new("git");
    c.env("GIT_TERMINAL_PROMPT", "0");
    c
}

fn with_worktrees_lock<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    use std::os::fd::AsRawFd;
    let Some(common) = common_git_dir(dir) else {
        return f();
    };
    let Ok(file) = fs::File::open(common) else {
        return f();
    };
    let deadline = Instant::now() + WORKTREE_LOCK_TIMEOUT;
    let locked = loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break true;
        }
        let err = std::io::Error::last_os_error();
        if !matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) {
            break false;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let out = f();
    if locked {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
    out
}

fn git_write(dir: &Path, args: &[&str]) -> Result<String, String> {
    let run = || {
        let out = git_cmd()
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!("git {} failed", args.join(" "))
            } else {
                err.chars().take(300).collect()
            })
        }
    };
    if matches!(args.first().copied(), Some("worktree" | "branch")) {
        with_worktrees_lock(dir, run)
    } else {
        run()
    }
}

fn git_read(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = git_cmd()
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut s) = stdout {
            let _ = std::io::Read::read_to_end(s, &mut buf);
        }
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut s) = stderr {
            let _ = std::io::Read::read_to_end(s, &mut buf);
        }
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if start.elapsed() < GIT_READ_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("git {} timed out", args.join(" ")));
            }
        }
    };
    let out_bytes = out_t.join().unwrap_or_default();
    let err_bytes = err_t.join().unwrap_or_default();
    let status = status?;
    if status.success() {
        Ok(String::from_utf8_lossy(&out_bytes).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&err_bytes).trim().to_string();
        Err(if err.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            err.chars().take(300).collect()
        })
    }
}

fn git_test(dir: &Path, args: &[&str]) -> Result<bool, String> {
    let out = git_cmd()
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

fn git_version() -> Option<String> {
    let out = git_cmd()
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let ver = text.split_whitespace().nth(2)?;
    let mut parts = ver.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some(format!("{major}.{minor}"))
}

fn merge_base(dir: &Path, a: &str, b: &str) -> Result<Option<String>, String> {
    let out = git_cmd()
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", a, b])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )),
        Some(1) if out.stderr.is_empty() => Ok(None),
        _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

fn patch_ids(dir: &Path, args: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut src = git_cmd()
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let Some(out) = src.stdout.take() else {
        let _ = src.wait();
        return Err("git produced no output".into());
    };
    let ids = git_cmd()
        .arg("-C")
        .arg(dir)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::from(out))
        .stderr(Stdio::null())
        .output();
    let src_ok = src.wait().is_ok_and(|s| s.success());
    let ids = ids.map_err(|e| e.to_string())?;
    if !src_ok || !ids.status.success() {
        return Err(format!("git {} failed", args.first().unwrap_or(&"")));
    }
    Ok(String::from_utf8_lossy(&ids.stdout)
        .lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            Some((p.next()?.to_string(), p.next()?.to_string()))
        })
        .collect())
}

fn landed_uncached(dir: &Path, head: &str, up: &str) -> Result<bool, String> {
    if git_test(dir, &["merge-base", "--is-ancestor", head, up])? {
        return Ok(true);
    }
    let Some(mb) = merge_base(dir, head, up)? else {
        return Ok(false);
    };
    let range = format!("{mb}..{up}");
    let upstream = patch_ids(dir, &["log", "--no-merges", "-p", "--no-color", &range])?;
    let has = |pid: &str| upstream.iter().any(|(p, _)| p == pid);
    let cherry = git_read(dir, &["cherry", up, head])?;
    if !cherry.is_empty() && cherry.lines().all(|l| l.starts_with('-')) {
        let tip = patch_ids(
            dir,
            &["show", "-p", "--no-color", "--format=commit %H", head],
        )?;
        if tip.first().is_some_and(|(p, _)| has(p)) {
            return Ok(true);
        }
    }
    let all = patch_ids(dir, &["diff-tree", "-p", "--no-color", &mb, head])?;
    Ok(all.first().is_some_and(|(p, _)| has(p)))
}

fn landed(repo: &Path, branch: &str, base: &str) -> bool {
    type Memo = HashMap<(PathBuf, String, String), bool>;
    static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
    let head = format!("refs/heads/{branch}");
    let (Ok(tip), Ok(at)) = (
        git_read(repo, &["rev-parse", "--verify", "--quiet", &head]),
        git_read(repo, &["rev-parse", "--verify", "--quiet", base]),
    ) else {
        return false;
    };
    let key = (repo.to_path_buf(), tip, at);
    let memo = MEMO.get_or_init(Default::default);
    if let Some(v) = memo.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return *v;
    }
    let Ok(v) = landed_uncached(repo, &head, base) else {
        return false;
    };
    let mut m = memo.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > LANDED_MEMO_CAP {
        m.clear();
    }
    m.insert(key, v);
    v
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorktreeTidy {
    Removed,
    Dirty,
    Unmerged,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchTidy {
    Deleted,
    Kept,
    Absent,
}

fn tidy_worktree(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    only_merged: bool,
    base: &str,
) -> (WorktreeTidy, BranchTidy) {
    let head_ref = format!("refs/heads/{branch}");
    let exists = git_read(repo, &["rev-parse", "--verify", "--quiet", &head_ref]).is_ok();
    let merged =
        exists && git_read(repo, &["merge-base", "--is-ancestor", &head_ref, base]).is_ok();
    let is_landed = merged || (exists && landed(repo, branch, base));
    let mut wt = WorktreeTidy::Absent;
    if worktree.exists() {
        wt = if only_merged && !is_landed {
            WorktreeTidy::Unmerged
        } else if git_write(repo, &["worktree", "remove", &worktree.to_string_lossy()]).is_ok() {
            WorktreeTidy::Removed
        } else {
            WorktreeTidy::Dirty
        };
    }
    let mut br = if exists {
        BranchTidy::Kept
    } else {
        BranchTidy::Absent
    };
    if exists
        && merged
        && !matches!(wt, WorktreeTidy::Dirty | WorktreeTidy::Unmerged)
        && git_write(repo, &["branch", "-d", branch]).is_ok()
    {
        br = BranchTidy::Deleted;
    }
    (wt, br)
}

fn why_kept(wt: &Path, keep_unmerged: bool) -> &'static str {
    match git_read(wt, &["status", "--porcelain"]).map(|s| !s.is_empty()) {
        Ok(true) => REASON_DIRTY,
        Ok(false) if keep_unmerged => REASON_UNMERGED,
        Ok(false) => REASON_REMOVE_FAILED,
        Err(_) => REASON_UNKNOWN_STATE,
    }
}

// ── RPC entry points ──

pub fn disk_inspect(req: &DiskInspectRequest) -> DiskInspectResponse {
    let root = PathBuf::from(&req.worktrees_root);
    let procs = process_dirs();
    let measure_entry = |target: &DiskInspectTarget, in_scratch: bool| -> DiskEntryMeasure {
        let wt = Path::new(&target.path);
        if !wt.is_dir() {
            return DiskEntryMeasure {
                item: target.item.clone(),
                exists: false,
                bytes: 0,
                cache_bytes: 0,
                caches: vec![],
                in_use: None,
            };
        }
        let caches = if in_scratch {
            scratch_cache_dirs(wt)
        } else {
            cache_dirs(wt)
        };
        let cache_bytes = caches.iter().map(|c| dir_bytes(c)).sum();
        let cache_names = caches
            .iter()
            .filter_map(|c| c.strip_prefix(wt).ok())
            .map(|c| c.to_string_lossy().to_string())
            .collect();
        DiskEntryMeasure {
            item: target.item.clone(),
            exists: true,
            bytes: dir_bytes(wt),
            cache_bytes,
            caches: cache_names,
            in_use: procs.as_deref().map(|p| in_use(wt, p)),
        }
    };
    let entries: Vec<DiskEntryMeasure> = req
        .entries
        .iter()
        .map(|t| measure_entry(t, false))
        .collect();
    let scratch: Vec<DiskEntryMeasure> =
        req.scratch.iter().map(|t| measure_entry(t, true)).collect();
    let known: Vec<PathBuf> = req.entries.iter().map(|e| PathBuf::from(&e.path)).collect();
    let mut orphans = vec![];
    if let Ok(rd) = fs::read_dir(&root) {
        for d in rd.flatten() {
            let p = d.path();
            if d.file_name() == TRASH || !p.is_dir() || known.contains(&p) {
                continue;
            }
            orphans.push(DiskOrphan {
                path: p.to_string_lossy().into_owned(),
                bytes: dir_bytes(&p),
            });
        }
    }
    orphans.sort_by(|a, b| a.path.cmp(&b.path));
    let sp = space::measure(&root);
    DiskInspectResponse {
        free: sp.as_ref().map(|s| s.free),
        total: sp.as_ref().map(|s| s.total),
        git: git_version(),
        trash: dir_bytes(&trash_dir(&root)),
        entries,
        scratch,
        orphans,
    }
}

pub fn disk_reclaim(req: &DiskReclaimRequest) -> DiskReclaimResponse {
    let root = PathBuf::from(&req.worktrees_root);
    let trash = trash_dir(&root);
    let free = || space::free_bytes(&root);
    let free_before = free();
    let mut freed = purge(&trash);
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    let mut deleted_branches = Vec::new();
    let mut kept_branches = Vec::new();
    let procs = process_dirs();

    for path in &req.git_projects {
        let p = Path::new(path);
        if p.is_dir()
            && let Err(e) = git_write(p, &["worktree", "prune"])
        {
            tracing::warn!("worktree prune {}: {e}", p.display());
        }
    }

    for c in &req.closed_worktrees {
        let wt = Path::new(&c.path);
        if !wt.is_dir() {
            continue;
        }
        if procs.as_deref().is_some_and(|p| in_use(wt, p)) {
            kept.push(DiskKept {
                item: c.item.clone(),
                reason: REASON_IN_USE.into(),
            });
            continue;
        }
        let _ = to_trash(&trash, &c.item, wt, &cache_dirs(wt));
        let repo = Path::new(&c.repo);
        if !c.repo.is_empty() && !c.branch.is_empty() {
            let (_wt_res, br_res) = tidy_worktree(repo, wt, &c.branch, c.keep_unmerged, "main");
            if wt.exists() {
                kept.push(DiskKept {
                    item: c.item.clone(),
                    reason: why_kept(wt, c.keep_unmerged).into(),
                });
            } else {
                removed.push(c.item.clone());
                match br_res {
                    BranchTidy::Deleted => {
                        deleted_branches.push((c.item.clone(), c.branch.clone()));
                    }
                    BranchTidy::Kept => {
                        if let Ok(head) = git_read(
                            repo,
                            &["rev-parse", "--short", &format!("refs/heads/{}", c.branch)],
                        ) {
                            kept_branches.push((
                                c.item.clone(),
                                c.path.clone(),
                                c.branch.clone(),
                                head,
                            ));
                        }
                    }
                    BranchTidy::Absent => {}
                }
            }
        } else if wt.exists() {
            kept.push(DiskKept {
                item: c.item.clone(),
                reason: why_kept(wt, c.keep_unmerged).into(),
            });
        } else {
            removed.push(c.item.clone());
        }
    }
    merge_freed(&mut freed, purge(&trash));

    for c in &req.idle_worktrees {
        if free().is_none_or(|f| f >= req.low_bytes) {
            break;
        }
        let wt = Path::new(&c.path);
        if !wt.is_dir() || procs.as_deref().is_none_or(|p| in_use(wt, p)) {
            continue;
        }
        let moved = to_trash(&trash, &c.item, wt, &cache_dirs(wt));
        if !moved.is_empty() {
            merge_freed(&mut freed, purge(&trash));
        }
    }

    if free().is_some_and(|f| f < req.low_bytes) {
        for c in &req.ended_scratch {
            if free().is_none_or(|f| f >= req.low_bytes) {
                break;
            }
            let dir = Path::new(&c.path);
            if !dir.is_dir() || procs.as_deref().is_none_or(|p| in_use(dir, p)) {
                continue;
            }
            let moved = to_trash(&trash, &c.item, dir, &scratch_cache_dirs(dir));
            if !moved.is_empty() {
                merge_freed(&mut freed, purge(&trash));
            }
        }
    }

    DiskReclaimResponse {
        free_before,
        free_after: free(),
        freed,
        removed,
        kept,
        deleted_branches,
        kept_branches,
    }
}
