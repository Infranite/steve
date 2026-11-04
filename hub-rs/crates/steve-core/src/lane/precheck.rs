//! What a merge request brings before it joins its lane: whether its change is on the target already
//! (ST-33 was merged and its card did not know), and how it overlaps the other requests of the lane
//! (ST-36 redid ST-34). Only exact evidence says "the same"; a partial overlap is reported with its
//! numbers and decided by whoever resolves the batch, never by a threshold here.
//!
//! Local git: fetch first ([`fetch`]), off the core's lock. A git that fails says
//! [`Presence::Unknown`] / [`Overlap::Unknown`], never "absent" or "different".

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::git::git;
pub use crate::upstream::{Presence, fetch};

/// How one request's change relates to another's (`this` against `other`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlap {
    /// The two bring the same: the same own diff (whitespace included, wherever it applies), or each
    /// adds nothing to the other.
    Same,
    /// This one adds nothing on top of the other: merging it onto the other gives the other's tree.
    InOther,
    /// The other adds nothing on top of this one.
    HasOther,
    /// Both touch `files`; `present` of the `added` lines this one adds there are in the other already.
    Partial {
        files: Vec<String>,
        present: usize,
        added: usize,
    },
    Unknown(String),
}

/// Another request of the lane, at the head it was frozen at.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub id: &'a str,
    pub head: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Precheck {
    pub presence: Presence,
    /// The requests this one overlaps (in the order given); the others share nothing with it.
    pub overlaps: Vec<(String, Overlap)>,
    /// The commit `target_ref` was at when checked: once the target moves on, check again.
    pub target_commit: Option<String>,
    /// Unix milliseconds.
    pub checked_at: i64,
}

/// `head` against the target `target_ref` (`refs/remotes/origin/main`) and the lane's `others`.
pub fn precheck(dir: &Path, target_ref: &str, head: &str, others: &[Request]) -> Precheck {
    let target_commit = git(dir, &["rev-parse", "--verify", "--quiet", target_ref]).ok();
    let presence = match &target_commit {
        Some(t) => crate::upstream::presence(dir, head, t),
        None => Presence::Unknown(format!("没有 {target_ref}")),
    };
    let overlaps = others
        .iter()
        .filter(|o| o.head != head)
        .filter_map(|o| {
            let x = match &target_commit {
                Some(t) => overlap(dir, t, head, o.head),
                None => Some(Overlap::Unknown(format!("没有 {target_ref}"))),
            };
            x.map(|x| (o.id.to_string(), x))
        })
        .collect();
    Precheck {
        presence,
        overlaps,
        target_commit,
        checked_at: crate::util::now(),
    }
}

/// How `this` relates to `other`, both merging into `target` (a commit); `None` when they share no file.
pub fn overlap(dir: &Path, target: &str, this: &str, other: &str) -> Option<Overlap> {
    match overlap_of(dir, target, this, other) {
        Ok(x) => x,
        Err(e) => Some(Overlap::Unknown(e)),
    }
}

fn overlap_of(
    dir: &Path,
    target: &str,
    this: &str,
    other: &str,
) -> Result<Option<Overlap>, String> {
    /* one of them shares no history with the target: it can't merge there, so it overlaps nothing */
    let (Some(base_a), Some(base_b)) = (
        crate::git::merge_base(dir, this, target)?,
        crate::git::merge_base(dir, other, target)?,
    ) else {
        return Ok(None);
    };
    /* equal patch ids only make two changes candidates (`--stable` ignores whitespace, which can be
    meaning); the same diff, whitespace and all, line numbers aside, makes them the same */
    let own = |base: &str, head: &str| -> Result<Option<String>, String> {
        Ok(
            crate::upstream::patch_ids(dir, &["diff-tree", "-p", "--no-color", base, head])?
                .into_iter()
                .next()
                .map(|(p, _)| p),
        )
    };
    if let (Some(a), Some(b)) = (own(&base_a, this)?, own(&base_b, other)?)
        && a == b
        && exact_diff(dir, &base_a, this)? == exact_diff(dir, &base_b, other)?
    {
        return Ok(Some(Overlap::Same));
    }
    let files = |base: &str, head: &str| -> Result<Vec<String>, String> {
        Ok(
            git(dir, &["diff", "--name-only", "--no-renames", base, head])?
                .lines()
                .map(String::from)
                .collect(),
        )
    };
    let theirs: HashSet<String> = files(&base_b, other)?.into_iter().collect();
    let shared: Vec<String> = files(&base_a, this)?
        .into_iter()
        .filter(|f| theirs.contains(f))
        .collect();
    if shared.is_empty() {
        return Ok(None);
    }
    let tree = |c: &str| git(dir, &["rev-parse", &format!("{c}^{{tree}}")]);
    let in_other = merges_to(dir, other, this)? == Some(tree(other)?);
    let has_other = merges_to(dir, this, other)? == Some(tree(this)?);
    match (in_other, has_other) {
        (true, true) => return Ok(Some(Overlap::Same)),
        (true, false) => return Ok(Some(Overlap::InOther)),
        (false, true) => return Ok(Some(Overlap::HasOther)),
        _ => {}
    }
    /* what this one adds in the shared files, and how much of it the other has already */
    let (mut present, mut added) = (0, 0);
    for f in &shared {
        let diff = git(
            dir,
            &[
                "diff",
                "-U0",
                "--no-color",
                "--no-renames",
                &base_a,
                this,
                "--",
                f,
            ],
        )?;
        /* a file the other one removed has nothing of this one; one git can't read is not known */
        let theirs = if git(dir, &["ls-tree", "--name-only", other, "--", f])?.is_empty() {
            String::new()
        } else {
            git(dir, &["show", &format!("{other}:{f}")])?
        };
        let theirs: HashSet<&str> = theirs.lines().map(str::trim).collect();
        for l in diff.lines() {
            let Some(l) = l.strip_prefix('+') else {
                continue;
            };
            if l.starts_with("++") || l.trim().is_empty() {
                continue;
            }
            added += 1;
            if theirs.contains(l.trim()) {
                present += 1;
            }
        }
    }
    Ok(Some(Overlap::Partial {
        files: shared,
        present,
        added,
    }))
}

/// The diff from `base` to `head` without what depends on where it applies (blob ids, line numbers).
fn exact_diff(dir: &Path, base: &str, head: &str) -> Result<String, String> {
    let d = git(
        dir,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            base,
            head,
        ],
    )?;
    Ok(d.lines()
        .filter(|l| !l.starts_with("index "))
        .map(
            |l| match l.strip_prefix("@@ ").and_then(|r| r.split_once(" @@")) {
                Some((_, rest)) => format!("@@{rest}"),
                None => l.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The tree `git merge-tree` makes of `ours` and `theirs`: `None` when they conflict.
fn merges_to(dir: &Path, ours: &str, theirs: &str) -> Result<Option<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-tree", "--write-tree", "--no-messages", ours, theirs])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .map(|t| t.trim().to_string())),
        Some(1) => Ok(None),
        _ => {
            Err(crate::util::js_head(String::from_utf8_lossy(&out.stderr).trim(), 300).to_string())
        }
    }
}
