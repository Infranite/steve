//! What Steve recognizes about a target before it batches anything: how changes get in (the host's
//! merge queue, a pull request, a push), what has to pass, how a merge becomes a release, which files
//! every change fights over (versions, release notes, migration numbers, lock files), and how busy
//! the target is. Deterministic rules on what the repository and its host say; every fact carries
//! the evidence it came from, and "nothing there" ([`How::Default`]) is kept apart from "couldn't
//! look" ([`How::Unknown`]).
//!
//! Reads the target branch as last fetched, and asks GitHub (`gh api`, with a deadline) for the
//! branch's rules. Never under the core's lock.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::target::{Target, target_of};
use crate::git::git;
use crate::repo::Release;

/// How long one `gh api` call may take.
const GH_DEADLINE: Duration = Duration::from_secs(20);
/// The days [`Pace`] counts.
const PACE_DAYS: i64 = 7;

/// Where a fact came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum How {
    /// Seen in the repository, its host or the project's settings.
    Found,
    /// Looked, found nothing: the safe default.
    Default,
    /// Couldn't look; the value is the safe default meanwhile.
    Unknown { why: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact<T> {
    pub value: T,
    pub how: How,
    pub evidence: Vec<String>,
}

/// How a change gets into the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Merge {
    /// The host's merge queue: it batches, Steve hands requests to it.
    HostQueue,
    /// A pull request is required: one per batch.
    HostPr,
    /// Pushed to the branch.
    Push,
}

/// What makes a merged change released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ship {
    /// A pushed version tag (its workflow builds the release).
    Tag,
    /// The project's release command, once per batch.
    Script,
    /// Nothing more: merged is done.
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    /// `required` (the host won't merge without it), `workflow` (runs on pull requests or the merge
    /// queue) or `suggested` (a command the repository has; not run until the user says so).
    pub from: &'static str,
}

/// Files every change edits in a way that collides, and how a batch settles them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Numbered {
    /// `version`, `notes`, `migrations` or `lockfile`.
    pub kind: &'static str,
    pub path: String,
    pub evidence: String,
    pub handling: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pace {
    pub days: i64,
    /// Changes that reached the branch (first-parent commits).
    pub landed: usize,
    /// Version tags created.
    pub releases: usize,
    pub authors: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub target: Target,
    /// The ref read and its commit (`origin/main 1a2b3c4`).
    pub reads: String,
    pub merge: Fact<Merge>,
    pub checks: Fact<Vec<Check>>,
    pub ship: Fact<Ship>,
    pub numbered: Vec<Numbered>,
    pub pace: Pace,
}

/// The profile of the target the working copy `dir` merges into (`branch`, or the default one);
/// `release` is the project's own release setting, `gh` the GitHub CLI.
pub fn profile(
    dir: &Path,
    branch: Option<&str>,
    release: Option<&Release>,
    gh: &Path,
) -> Result<Profile, String> {
    let target = target_of(dir, branch)?;
    let remote = format!("refs/remotes/origin/{}", target.branch);
    let local = format!("refs/heads/{}", target.branch);
    let at = if crate::git::git_test(dir, &["rev-parse", "--verify", "--quiet", &remote])? {
        remote
    } else if crate::git::git_test(dir, &["rev-parse", "--verify", "--quiet", &local])? {
        local
    } else {
        return Err(format!("没有 {} 分支", target.branch));
    };
    let commit = git(dir, &["rev-parse", &at])?;
    let reads = format!(
        "{} {}",
        at.trim_start_matches("refs/remotes/")
            .trim_start_matches("refs/heads/"),
        &commit[..commit.len().min(7)]
    );
    let files: Vec<String> = git(dir, &["ls-tree", "-r", "--name-only", &commit])?
        .lines()
        .map(String::from)
        .collect();
    let show = |f: &str| git(dir, &["show", &format!("{commit}:{f}")]);

    let host = host_of(&target);
    let rules = match &host {
        Host::GitHub(slug) => Some(github_rules(gh, slug, &target.branch)),
        _ => None,
    };
    let workflows = workflows(&files, &show);

    Ok(Profile {
        merge: merge_fact(&host, rules.as_ref()),
        checks: checks_fact(rules.as_ref(), &workflows, &files, &show),
        ship: ship_fact(release, &workflows),
        numbered: numbered(dir, &files),
        pace: pace(dir, &commit)?,
        reads,
        target,
    })
}

/* ── the host ── */

enum Host {
    Local,
    GitHub(String),
    Other(String),
}

fn host_of(t: &Target) -> Host {
    if t.upstream.starts_with("file:") {
        return Host::Local;
    }
    match t.upstream.split_once('/') {
        Some(("github.com", slug)) => Host::GitHub(slug.to_string()),
        Some((h, _)) => Host::Other(h.to_string()),
        None => Host::Other(t.upstream.clone()),
    }
}

/// What GitHub says about the branch.
#[derive(Debug, Default)]
struct Rules {
    queue: bool,
    pr: bool,
    required: BTreeSet<String>,
    evidence: Vec<String>,
}

fn github_rules(gh: &Path, slug: &str, branch: &str) -> Result<Rules, String> {
    let mut r = Rules::default();
    /* rulesets (the only place a merge queue is set up) */
    let rules = gh_api(gh, &format!("repos/{slug}/rules/branches/{branch}"))
        .map_err(|e| format!("读不到 GitHub 上 {branch} 的规则：{}", e.text()))?;
    for x in rules.as_array().into_iter().flatten() {
        match x["type"].as_str() {
            Some("merge_queue") => r.queue = true,
            Some("pull_request") => r.pr = true,
            Some("required_status_checks") => {
                for c in x["parameters"]["required_status_checks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if let Some(n) = c["context"].as_str() {
                        r.required.insert(n.into());
                    }
                }
            }
            _ => {}
        }
    }
    /* classic branch protection */
    let b = gh_api(gh, &format!("repos/{slug}/branches/{branch}"))
        .map_err(|e| format!("读不到 GitHub 上的 {branch} 分支：{}", e.text()))?;
    if b["protected"].as_bool() == Some(true) {
        match gh_api(gh, &format!("repos/{slug}/branches/{branch}/protection")) {
            Ok(p) => {
                if p.get("required_pull_request_reviews").is_some() {
                    r.pr = true;
                }
                for c in p["required_status_checks"]["contexts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if let Some(n) = c.as_str() {
                        r.required.insert(n.into());
                    }
                }
            }
            Err(e) => {
                return Err(format!(
                    "{branch} 有分支保护，但读不到细节（需要仓库管理员权限）：{}",
                    e.text()
                ));
            }
        }
    }
    if r.queue {
        r.evidence.push(format!("GitHub：{branch} 开了合并队列"));
    }
    if r.pr {
        r.evidence
            .push(format!("GitHub：{branch} 要求经过 PR 合并"));
    }
    if !r.queue && !r.pr {
        r.evidence
            .push(format!("GitHub：{branch} 没有要求 PR，也没有合并队列"));
    }
    Ok(r)
}

fn merge_fact(host: &Host, rules: Option<&Result<Rules, String>>) -> Fact<Merge> {
    match (host, rules) {
        (Host::Local, _) => Fact {
            value: Merge::Push,
            how: How::Found,
            evidence: vec!["只在本机，没有托管方".into()],
        },
        (_, Some(Ok(r))) => Fact {
            value: if r.queue {
                Merge::HostQueue
            } else if r.pr {
                Merge::HostPr
            } else {
                Merge::Push
            },
            how: How::Found,
            evidence: r.evidence.clone(),
        },
        (_, Some(Err(e))) => unknown(Merge::Push, e.clone()),
        (Host::Other(h), None) | (Host::GitHub(h), None) => {
            unknown(Merge::Push, format!("还认不出 {h} 上的合并设置"))
        }
    }
}

fn unknown<T>(value: T, why: String) -> Fact<T> {
    Fact {
        value,
        how: How::Unknown { why },
        evidence: vec![],
    }
}

/* ── checks ── */

/// A workflow file and the events that start it (`push` → `["tags", "branches"]` …); `None` when its
/// `on:` can't be read.
struct Workflow {
    file: String,
    on: Option<BTreeMap<String, Vec<String>>>,
}

fn workflows(files: &[String], show: &dyn Fn(&str) -> Result<String, String>) -> Vec<Workflow> {
    files
        .iter()
        .filter(|f| {
            f.starts_with(".github/workflows/") && (f.ends_with(".yml") || f.ends_with(".yaml"))
        })
        .map(|f| Workflow {
            file: f.clone(),
            on: show(f).ok().and_then(|y| triggers(&y)),
        })
        .collect()
}

fn checks_fact(
    rules: Option<&Result<Rules, String>>,
    workflows: &[Workflow],
    files: &[String],
    show: &dyn Fn(&str) -> Result<String, String>,
) -> Fact<Vec<Check>> {
    let mut value = vec![];
    let mut evidence = vec![];
    if let Some(Ok(r)) = rules {
        for n in &r.required {
            value.push(Check {
                name: n.clone(),
                from: "required",
            });
        }
        if !r.required.is_empty() {
            evidence.push(format!("GitHub 要求通过：{}", join(&r.required)));
        }
    }
    for w in workflows {
        match &w.on {
            Some(on)
                if on.contains_key("pull_request")
                    || on.contains_key("pull_request_target")
                    || on.contains_key("merge_group") =>
            {
                value.push(Check {
                    name: w.file.clone(),
                    from: "workflow",
                });
                evidence.push(format!(
                    "{} 在 {} 时运行",
                    w.file,
                    join(
                        on.keys()
                            .filter(|k| k.starts_with("pull_request") || *k == "merge_group")
                    )
                ));
            }
            Some(_) => {}
            None => evidence.push(format!("认不出 {} 的触发条件", w.file)),
        }
    }
    let found = !value.is_empty();
    for (cmd, why) in suggested(files, show) {
        value.push(Check {
            name: cmd,
            from: "suggested",
        });
        evidence.push(why);
    }
    let how = match rules {
        Some(Err(e)) => How::Unknown { why: e.clone() },
        _ if found => How::Found,
        _ => How::Default,
    };
    if how == How::Default {
        evidence.insert(0, "合并前没有托管方的检查：集成成功就算可合并".into());
    }
    Fact {
        value,
        how,
        evidence,
    }
}

/// Commands the repository has for checking itself, from its manifests: never run until the user
/// takes them.
fn suggested(
    files: &[String],
    show: &dyn Fn(&str) -> Result<String, String>,
) -> Vec<(String, String)> {
    let mut out = vec![];
    let shallow = |f: &str, name: &str| {
        f == name || (f.ends_with(&format!("/{name}")) && f.matches('/').count() == 1)
    };
    for f in files.iter().filter(|f| shallow(f, "Cargo.toml")) {
        let dir = f.trim_end_matches("Cargo.toml").trim_end_matches('/');
        let workspace = show(f).is_ok_and(|t| t.lines().any(|l| l.trim() == "[workspace]"));
        if workspace || dir.is_empty() {
            let at = if dir.is_empty() {
                String::new()
            } else {
                format!(" --manifest-path {f}")
            };
            out.push((
                format!("cargo test{at} --workspace"),
                format!("{f} 是 Rust 工作区"),
            ));
        }
    }
    for f in files.iter().filter(|f| shallow(f, "package.json")) {
        let Ok(v) =
            show(f).and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        else {
            continue;
        };
        let dir = f.trim_end_matches("package.json").trim_end_matches('/');
        let pre = if dir.is_empty() {
            String::new()
        } else {
            format!("npm --prefix {dir} ")
        };
        for s in ["check", "test", "lint"] {
            if v["scripts"][s].is_string() {
                out.push((
                    format!(
                        "{}{}",
                        if pre.is_empty() { "npm " } else { &pre },
                        if s == "test" {
                            "test".to_string()
                        } else {
                            format!("run {s}")
                        }
                    ),
                    format!("{f} 有 {s} 脚本"),
                ));
            }
        }
    }
    out
}

/// The events of a GitHub workflow's `on:`, with the keys under each (`push: tags:` …). A small reader
/// for the three forms GitHub accepts (a name, a list, a mapping); `None` when there is no `on:` it
/// can read.
fn triggers(yaml: &str) -> Option<BTreeMap<String, Vec<String>>> {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let lines: Vec<&str> = yaml
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .collect();
    let at = lines.iter().position(|l| {
        indent(l) == 0
            && ["on:", "\"on\":", "'on':", "true:"]
                .iter()
                .any(|k| l.starts_with(k))
    })?;
    let rest = lines[at].split_once(':')?.1;
    let rest = rest.split(" #").next().unwrap_or("").trim();
    let mut out = BTreeMap::new();
    let names = |s: &str| -> Vec<String> {
        s.trim_matches(|c| c == '[' || c == ']' || c == '{' || c == '}')
            .split(',')
            .map(|x| {
                x.split(':')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches(['"', '\''])
                    .to_string()
            })
            .filter(|x| !x.is_empty())
            .collect()
    };
    if !rest.is_empty() {
        for n in names(rest) {
            out.insert(n, vec![]);
        }
        return Some(out);
    }
    let block: Vec<&str> = lines[at + 1..]
        .iter()
        .take_while(|l| indent(l) > 0)
        .copied()
        .collect();
    let first = indent(block.first()?);
    let mut current: Option<String> = None;
    for l in block {
        let t = l.trim();
        if indent(l) == first {
            if let Some(item) = t.strip_prefix("- ") {
                out.insert(item.trim().trim_matches(['"', '\'']).to_string(), vec![]);
                current = None;
                continue;
            }
            let (k, v) = t.split_once(':')?;
            let k = k.trim().trim_matches(['"', '\'']).to_string();
            let v = v.split(" #").next().unwrap_or("").trim();
            let subs = if v.starts_with('{') { names(v) } else { vec![] };
            out.insert(k.clone(), subs);
            current = Some(k);
        } else if let Some(k) = &current
            && let Some((sub, _)) = t.split_once(':')
            && !t.starts_with("- ")
            && indent(l) > first
        {
            let subs = out.entry(k.clone()).or_default();
            let sub = sub.trim().to_string();
            /* only the first level under the event */
            if !subs.contains(&sub) && !sub.contains(' ') {
                subs.push(sub);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/* ── releasing ── */

fn ship_fact(release: Option<&Release>, workflows: &[Workflow]) -> Fact<Ship> {
    let tag_flows: Vec<&str> = workflows
        .iter()
        .filter(|w| {
            w.on.as_ref()
                .and_then(|on| on.get("push"))
                .is_some_and(|s| s.iter().any(|k| k == "tags"))
        })
        .map(|w| w.file.as_str())
        .collect();
    let mut evidence: Vec<String> = tag_flows
        .iter()
        .map(|f| format!("{f} 在推送 tag 时运行"))
        .collect();
    let done = release.and_then(|r| r.done_when.as_deref());
    let cmd = release.map(|r| r.cmd.trim()).filter(|c| !c.is_empty());
    let value = match (done, cmd) {
        (Some(d), _) if d == crate::upstream::DONE_TAG => {
            evidence.insert(0, "项目设置：完成标准是 PR 合并并推送 tag".into());
            Ship::Tag
        }
        (Some(d), _) if d == crate::upstream::DONE_MERGE => {
            evidence.insert(0, "项目设置：完成标准是 PR 合并".into());
            Ship::Merge
        }
        (_, Some(c)) => {
            evidence.insert(0, format!("项目设置：发布命令 {c}"));
            Ship::Script
        }
        _ if !tag_flows.is_empty() => Ship::Tag,
        _ => {
            return Fact {
                value: Ship::Merge,
                how: How::Default,
                evidence: vec!["没有发布命令，也没有按 tag 发布的 workflow：合并就算完成".into()],
            };
        }
    };
    Fact {
        value,
        how: How::Found,
        evidence,
    }
}

/* ── what every change fights over ── */

fn numbered(dir: &Path, files: &[String]) -> Vec<Numbered> {
    let mut out = vec![];
    let has = |f: &str| files.iter().any(|x| x == f);
    let cut = has("ops/cut-release.py");

    let tags: Vec<String> = git(dir, &["tag", "-l", "v*", "--sort=-v:refname"])
        .unwrap_or_default()
        .lines()
        .filter(|t| semver(t.trim_start_matches('v')))
        .map(String::from)
        .collect();
    if let Some(last) = tags.first() {
        out.push(Numbered {
            kind: "version",
            path: "tag vX.Y.Z".into(),
            evidence: format!("最新 {last}，共 {} 个", tags.len()),
            handling: if cut {
                "发布时由 ops/cut-release.py 取下一个号".into()
            } else {
                "没有发布工具：谁打 tag 谁定版本，可能撞号".into()
            },
        });
    }

    /* release notes: a directory of X.Y.Z.md, or a CHANGELOG */
    let mut notes: BTreeMap<String, usize> = BTreeMap::new();
    for f in files {
        let (d, name) = f.rsplit_once('/').unwrap_or(("", f));
        if let Some(v) = name.strip_suffix(".md")
            && semver(v)
        {
            *notes.entry(d.to_string()).or_default() += 1;
        }
    }
    for (d, n) in notes {
        let fragments = files
            .iter()
            .any(|f| f.starts_with(&format!("{d}/unreleased/")));
        out.push(Numbered {
            kind: "notes",
            path: format!("{d}/"),
            evidence: format!("{n} 份按版本写的说明"),
            handling: if fragments {
                format!("片段写在 {d}/unreleased/，发布时拼成一份")
            } else {
                "在分支上按版本写，会撞号".into()
            },
        });
    }
    for f in files
        .iter()
        .filter(|f| f.rsplit('/').next() == Some("CHANGELOG.md"))
    {
        out.push(Numbered {
            kind: "notes",
            path: f.clone(),
            evidence: "所有改动都写同一个文件".into(),
            handling: "合并时容易冲突".into(),
        });
    }

    /* migrations: numbered files in a `migrations` directory */
    let mut migs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in files {
        let (d, name) = f.rsplit_once('/').unwrap_or(("", f));
        if d.rsplit('/').next() == Some("migrations")
            && name.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            migs.entry(d.to_string())
                .or_default()
                .push(name.to_string());
        }
    }
    for (d, mut names) in migs {
        names.sort();
        let lock = has(&format!("{d}/LOCK"));
        out.push(Numbered {
            kind: "migrations",
            path: format!("{d}/"),
            evidence: format!(
                "{} 个，最新 {}{}",
                names.len(),
                names.last().map(String::as_str).unwrap_or(""),
                if lock { "，有 LOCK" } else { "" }
            ),
            handling: "合并时取下一个空号".into(),
        });
    }

    const LOCKS: &[&str] = &[
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "poetry.lock",
        "uv.lock",
        "go.sum",
    ];
    /* the ones the repository is built from: at the top or one directory down, like its manifests
    (deeper ones are fixtures and experiments) */
    for f in files.iter().filter(|f| f.matches('/').count() <= 1) {
        let name = f.rsplit('/').next().unwrap_or(f);
        if LOCKS.contains(&name) {
            out.push(Numbered {
                kind: "lockfile",
                path: f.clone(),
                evidence: "依赖锁文件".into(),
                handling: "冲突时用工具重新生成，不手改".into(),
            });
        }
    }
    out
}

fn semver(v: &str) -> bool {
    let p: Vec<&str> = v.split('.').collect();
    p.len() == 3
        && p.iter()
            .all(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
}

/* ── pace ── */

fn pace(dir: &Path, commit: &str) -> Result<Pace, String> {
    let since = crate::util::now() / 1000 - PACE_DAYS * 86_400;
    let log = git(
        dir,
        &[
            "log",
            "--first-parent",
            &format!("--since={since}"),
            "--format=%an",
            commit,
        ],
    )?;
    let authors: BTreeSet<&str> = log.lines().collect();
    let tags = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(creatordate:unix) %(refname:short)",
            "refs/tags",
        ],
    )?;
    let releases = tags
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter(|(t, n)| {
            t.parse::<i64>().is_ok_and(|t| t >= since) && semver(n.trim_start_matches('v'))
        })
        .count();
    Ok(Pace {
        days: PACE_DAYS,
        landed: log.lines().count(),
        releases,
        authors: authors.len(),
    })
}

/* ── gh ── */

enum GhError {
    NotFound(String),
    Other(String),
}

impl GhError {
    fn text(&self) -> &str {
        match self {
            GhError::NotFound(s) | GhError::Other(s) => s,
        }
    }
}

/// `gh api path` as JSON, with [`GH_DEADLINE`]; output drained while it runs.
fn gh_api(gh: &Path, path: &str) -> Result<Value, GhError> {
    let mut child = Command::new(gh)
        .args(["api", "-H", "Accept: application/vnd.github+json", path])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| GhError::Other(format!("gh 不可用：{e}")))?;
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let _ = r.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    );
    let end = Instant::now() + GH_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (out, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    match status {
        Some(s) if s.success() => {
            serde_json::from_str(&out).map_err(|e| GhError::Other(e.to_string()))
        }
        Some(_) => {
            let msg = crate::util::js_head(err.trim(), 200).to_string();
            Err(if err.contains("HTTP 404") {
                GhError::NotFound(msg)
            } else {
                GhError::Other(msg)
            })
        }
        None => Err(GhError::Other("gh 超时".into())),
    }
}

fn join<'a>(xs: impl IntoIterator<Item = &'a String>) -> String {
    xs.into_iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("、")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(y: &str) -> Vec<(String, Vec<String>)> {
        triggers(y).unwrap().into_iter().collect()
    }

    #[test]
    fn workflow_triggers_in_every_form_github_accepts() {
        assert_eq!(keys("on: push\njobs:\n"), vec![("push".into(), vec![])]);
        assert_eq!(
            keys("name: CI\non: [push, pull_request]\n"),
            vec![("pull_request".into(), vec![]), ("push".into(), vec![])]
        );
        assert_eq!(
            keys(
                "name: Release\non:\n  push:\n    tags: ['v[0-9]*']\n    branches:\n      - main\n  workflow_dispatch:\npermissions:\n  contents: read\n"
            ),
            vec![
                ("push".into(), vec!["tags".into(), "branches".into()]),
                ("workflow_dispatch".into(), vec![])
            ]
        );
        assert_eq!(
            keys("\"on\":\n  - pull_request\n  - merge_group\n"),
            vec![
                ("merge_group".into(), vec![]),
                ("pull_request".into(), vec![])
            ]
        );
        assert_eq!(
            keys("on:\n  push: {tags: ['v*']}\n"),
            vec![("push".into(), vec!["tags".into()])]
        );
        assert_eq!(triggers("name: x\njobs:\n  a:\n"), None);
    }

    #[test]
    fn a_change_of_semver_shape_only() {
        assert!(semver("0.2.26"));
        assert!(!semver("0.2"));
        assert!(!semver("0.2.x"));
    }
}
