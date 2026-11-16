//! The item's own branch and pull request (owner's decision of 2026-11-17, #219): the item's agent
//! may, without a card, push its **own** branch to `origin`, create, edit or comment on that
//! branch's pull request with `gh`, and open an issue (`gh issue create`). Everything else hard
//! limit 5 covers stays as it was: another remote, branch, refspec destination or tag, a force push,
//! deleting, `--tags`/`--all`/`--mirror`, `gh pr merge`, `gh repo`, `gh release`, `gh api` writes.
//! Branch protection on the default branch is the backstop.
//!
//! The own branch is the one recorded for the item (`items.branch`, `steve/…`), never one read off
//! the command. Only a command written out on the request's own line counts: `git` or `gh` as its
//! first word, no variable, no assignment in front, no wrapper, nothing that another command runs
//! (`sh -c`, `env -S`, `xargs`, `find -exec`…). It runs in the item's own checkout (the item's
//! directory or below it, no repository nested in between, `-C` the same), and no other git option
//! comes before `push`. The accepted `git push` forms are exactly:
//!
//! - `git push origin <own>`, `git push origin <own>:<own>`, with `-u`/`--set-upstream` anywhere;
//! - `git push origin HEAD`, `git push origin`, `git push`, with `-u`/`--set-upstream` anywhere, when
//!   the checkout's `HEAD` is on the own branch as read when the request came ([`Checkout`]) and no
//!   other part of the line can move it ([`keeps_head`]). The last two push where git's settings
//!   say, so those are read too: `push.default` is `simple` or `current` (or `upstream` whose
//!   upstream is the own branch), and a plain `git push` goes to `origin`.
//!
//! With the settings known, none of the forms goes when `remote.origin.push` or
//! `remote.origin.mirror` is set (a refspec of the remote's own could send the branch elsewhere).
//!
//! Nothing here runs anything: [`Checkout`] is read by the executor ([`super::facts`]).

use std::path::Path;

use serde::Serialize;

use super::limits::Places;
use super::shell::{self, Parsed, Simple};

/// The item's checkout as it was when the request came.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Checkout {
    /// The branch `HEAD` is on (`None`: detached, or not known).
    pub head: Option<String>,
    /// Its effective git settings (`git config --list`), the section and key lower-cased as git
    /// writes them; `None` when they could not be read (a workspace on another Node).
    pub config: Option<Vec<(String, String)>>,
}

impl Checkout {
    /// `git symbolic-ref --short -q HEAD` (empty: detached) and `git config --list -z`.
    pub fn from_git(head: &str, config: Option<&str>) -> Self {
        let head = head.trim();
        Checkout {
            head: (!head.is_empty()).then(|| head.to_string()),
            config: config.map(|c| {
                c.split('\0')
                    .filter(|e| !e.is_empty())
                    .map(|e| match e.split_once('\n') {
                        Some((k, v)) => (k.to_string(), v.to_string()),
                        // a key with no value is a boolean true
                        None => (e.to_string(), "true".to_string()),
                    })
                    .collect()
            }),
        }
    }

    /// The last value of `section.<sub>.key` (git's rule: the last one counts); the section and key
    /// compare without case, the subsection exactly.
    fn get(&self, section: &str, sub: Option<&str>, key: &str) -> Option<&str> {
        let want = match sub {
            Some(s) => format!("{}.{s}.{}", section.to_lowercase(), key.to_lowercase()),
            None => format!("{}.{}", section.to_lowercase(), key.to_lowercase()),
        };
        self.config
            .as_ref()?
            .iter()
            .rev()
            .find(|(k, _)| {
                // git writes the section and the key lower-cased; the subsection as it is
                match (k.split_once('.'), k.rsplit_once('.')) {
                    (Some((s, _)), Some((rest, name))) => {
                        let mid = rest.get(s.len()..).unwrap_or("");
                        format!("{}{mid}.{}", s.to_lowercase(), name.to_lowercase()) == want
                    }
                    _ => false,
                }
            })
            .map(|(_, v)| v.as_str())
    }

    /// `remote.origin.push` or `remote.origin.mirror`: a push to `origin` may not go where its
    /// refspec says. `None`: not known.
    fn origin_rewrites(&self) -> Option<bool> {
        self.config.as_ref()?;
        let mirror = self
            .get("remote", Some("origin"), "mirror")
            .is_some_and(|v| {
                !matches!(v.to_lowercase().as_str(), "false" | "no" | "off" | "0" | "")
            });
        Some(mirror || self.get("remote", Some("origin"), "push").is_some())
    }

    /// Why a push without a refspec might not push `own` to the same branch of `origin`; `bare`: no
    /// remote either (`git push`).
    fn default_push(&self, own: &str, bare: bool) -> Result<(), String> {
        if self.config.is_none() {
            return Err("看不到这个检出的 git 设置，不知道不带分支的 git push 会推到哪".into());
        }
        if bare {
            let remote = self
                .get("branch", Some(own), "pushRemote")
                .or_else(|| self.get("remote", None, "pushDefault"))
                .or_else(|| self.get("branch", Some(own), "remote"))
                .unwrap_or("origin");
            if remote != "origin" {
                return Err(format!("不带远端的 git push 会推到 {remote}，不是 origin"));
            }
        }
        let mode = self
            .get("push", None, "default")
            .unwrap_or("simple")
            .to_lowercase();
        let upstream_is_own =
            self.get("branch", Some(own), "merge") == Some(format!("refs/heads/{own}").as_str());
        match mode.as_str() {
            "simple" | "current" => Ok(()),
            "upstream" | "tracking" if upstream_is_own => Ok(()),
            m => Err(format!(
                "push.default 是 {m}，不带分支的 git push 不只推自己的分支"
            )),
        }
    }
}

/// What the item owns that a command is judged against, as it reaches one command.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Own<'a> {
    /// The item's own branch (`items.branch`).
    pub branch: Option<&'a str>,
    /// Its checkout when the request came.
    pub checkout: Option<&'a Checkout>,
    /// The command is written out on the request's own line: not run by another command, no
    /// variable, assignment or wrapper in front.
    pub top: bool,
    /// No other part of the line can move `HEAD` or where a push goes ([`keeps_head`]).
    pub head_kept: bool,
}

impl<'a> Own<'a> {
    pub fn new(branch: Option<&'a str>, checkout: Option<&'a Checkout>) -> Self {
        Own {
            branch,
            checkout,
            top: false,
            head_kept: false,
        }
    }

    /// The commands of the request's own line `p`.
    pub fn line(self, p: &Parsed) -> Self {
        Own {
            top: p.opaque.is_empty(),
            head_kept: keeps_head(p),
            ..self
        }
    }

    /// A command something else runs: only the branch itself still counts.
    pub fn nested(self) -> Self {
        Own {
            top: false,
            head_kept: false,
            ..self
        }
    }

    /// `c` on this line: still the line's own only when it is written out as it runs.
    pub fn at(self, c: &Simple) -> Self {
        if self.top && direct(c) {
            self
        } else {
            self.nested()
        }
    }

    /// The own branch, when it is one Steve made (`steve/…`): never a default branch.
    fn own(&self) -> Result<&'a str, String> {
        match self.branch {
            Some(b) if is_item_branch(b) => Ok(b),
            Some(b) => Err(format!("{b} 不是 Steve 给事项开的分支")),
            None => Err("这件事项没有自己的分支".into()),
        }
    }

    /// The branch `HEAD` is on is the own branch, and stays so for the whole line.
    fn head_is_own(&self, own: &str) -> Result<(), String> {
        if !self.head_kept {
            return Err("这一行里还有别的命令可能切换分支，看不出推的是哪个分支".into());
        }
        match self.checkout.and_then(|c| c.head.as_deref()) {
            Some(h) if h == own => Ok(()),
            Some(h) => Err(format!("当前检出的是 {h}，不是这件事项的分支 {own}")),
            None => Err("看不出当前检出的是哪个分支".into()),
        }
    }
}

/// `steve/<id>`: the branches Steve names items' branches with.
fn is_item_branch(b: &str) -> bool {
    b.strip_prefix("steve/").is_some_and(|r| {
        !r.is_empty()
            && r.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
            && !r.contains("..")
    })
}

/// `git` or `gh` as written, by its name (looked up on `PATH`, not a file of the agent's), with no
/// variable, assignment, wrapper or here-document.
fn direct(c: &Simple) -> bool {
    !c.expands && c.stdin.is_none() && c.words.first().is_some_and(|w| w == "git" || w == "gh")
}

/// git subcommands that leave `HEAD` on its branch.
const KEEPS_HEAD: &[&str] = &[
    "add",
    "commit",
    "status",
    "diff",
    "log",
    "show",
    "fetch",
    "rev-parse",
    "ls-files",
    "grep",
    "blame",
    "describe",
    "show-ref",
    "rev-list",
    "merge-base",
    "cat-file",
    "shortlog",
    "restore",
    "rm",
    "mv",
    "push",
];
/// gh subcommands of `pr`/`issue` that leave the checkout as it is.
const GH_KEEPS_HEAD: &[&str] = &[
    "create", "edit", "comment", "view", "list", "status", "checks", "diff",
];

/// No part of line `p` can move `HEAD` or where a push goes: every git command is one of
/// [`KEEPS_HEAD`] with no option before it, every gh command one of [`GH_KEEPS_HEAD`], nothing sets
/// a `GIT_`/`GH_` variable or the shell's state, and no other command so much as names git.
pub(crate) fn keeps_head(p: &Parsed) -> bool {
    p.cmds.iter().all(|c| {
        if c.words
            .iter()
            .any(|w| w.starts_with("GIT_") || w.starts_with("GH_"))
        {
            return false;
        }
        let Some(a) = shell::argv(&c.words) else {
            return true;
        };
        let first = |i: usize| a.args.get(i).map(String::as_str).unwrap_or("");
        match a.program.as_str() {
            "git" => KEEPS_HEAD.contains(&first(0)),
            "gh" => matches!(first(0), "pr" | "issue") && GH_KEEPS_HEAD.contains(&first(1)),
            "export" | "set" | "unset" | "source" | "." | "eval" | "alias" | "declare"
            | "typeset" | "exec" => false,
            _ => !c
                .words
                .iter()
                .any(|w| w.contains("git") || shell::basename(w) == "gh"),
        }
    })
}

/// The command runs in the item's own checkout: `dir` (`git -C`), or where the line is, inside the
/// item's directory with no repository of its own in between.
fn in_own_checkout(pl: &Places, dir: Option<&str>) -> Result<(), String> {
    let Some(root) = pl.cwd.as_deref() else {
        return Err("这件事项没有自己的工作区".into());
    };
    let here = match dir {
        Some(d) => pl.resolve(d),
        None => pl.base.clone(),
    };
    let Some(here) = here.filter(|h| h.starts_with(root)) else {
        return Err("不是在这件事项自己的检出里运行".into());
    };
    let mut p: &Path = &here;
    while p != root {
        if p.join(".git").exists() {
            return Err(format!("{} 是另一个仓库", p.display()));
        }
        match p.parent() {
            Some(up) => p = up,
            None => break,
        }
    }
    Ok(())
}

/// `git <args>` when it is a push of the item's own branch to `origin` that goes without a card;
/// otherwise why not (also when it is no push at all).
pub(crate) fn git_push(args: &[String], pl: &Places, own: Own) -> Result<(), String> {
    let mut i = 0;
    let mut dir = None;
    while let Some(x) = args.get(i) {
        match x.as_str() {
            "-C" if dir.is_none() => {
                dir = args.get(i + 1).map(String::as_str);
                i += 2;
            }
            s if s.starts_with('-') => return Err(format!("git push 前面有选项 {s}")),
            _ => break,
        }
    }
    if args.get(i).map(String::as_str) != Some("push") {
        return Err("不是 git push".into());
    }
    let b = own.own()?;
    if !own.top {
        return Err("git push 不是直接写在这一行上的命令".into());
    }
    in_own_checkout(pl, dir)?;
    let mut pos = Vec::new();
    for x in &args[i + 1..] {
        match x.as_str() {
            "-u" | "--set-upstream" => {}
            s if s.starts_with('-') => return Err(format!("git push {s}")),
            s => pos.push(s),
        }
    }
    if let Some(r) = pos.first()
        && *r != "origin"
    {
        return Err(format!("推到 {r}，不是 origin"));
    }
    if own
        .checkout
        .and_then(Checkout::origin_rewrites)
        .unwrap_or(false)
    {
        return Err("origin 设了 push 或 mirror，推送不一定按命令写的去".into());
    }
    let pair = format!("{b}:{b}");
    match pos.as_slice() {
        [] | [_] => {
            own.head_is_own(b)?;
            match own.checkout {
                Some(c) => c.default_push(b, pos.is_empty()),
                None => Err("看不到这个检出的 git 设置，不知道不带分支的 git push 会推到哪".into()),
            }
        }
        [_, r] if *r == b || *r == pair => Ok(()),
        [_, "HEAD"] => own.head_is_own(b),
        [_, r] => Err(format!("git push 的 {r} 不是这件事项的分支 {b}")),
        _ => Err("git push 推了不止一个分支".into()),
    }
}

/// The options of one gh command: long names that take a value, short letters that take one,
/// long and short ones that do not.
struct Flags {
    valued: &'static [&'static str],
    short_valued: &'static str,
    bools: &'static [&'static str],
    short_bools: &'static str,
}

/// `gh pr create`. Not `--web`, `--editor`, `--recover` or `--repo`.
const PR_CREATE: Flags = Flags {
    valued: &[
        "assignee",
        "base",
        "body",
        "body-file",
        "head",
        "label",
        "milestone",
        "project",
        "reviewer",
        "template",
        "title",
    ],
    short_valued: "aBbFHlmprTt",
    bools: &[
        "draft",
        "dry-run",
        "fill",
        "fill-first",
        "fill-verbose",
        "no-maintainer-edit",
    ],
    short_bools: "df",
};
/// `gh pr edit`. Not `--repo`.
const PR_EDIT: Flags = Flags {
    valued: &[
        "add-assignee",
        "add-label",
        "add-project",
        "add-reviewer",
        "base",
        "body",
        "body-file",
        "milestone",
        "remove-assignee",
        "remove-label",
        "remove-project",
        "remove-reviewer",
        "title",
    ],
    short_valued: "BbFmt",
    bools: &["remove-milestone"],
    short_bools: "",
};
/// `gh pr comment`. Not `--delete-last`, `--web`, `--editor` or `--repo`.
const PR_COMMENT: Flags = Flags {
    valued: &["body", "body-file"],
    short_valued: "bF",
    bools: &["edit-last", "create-if-none"],
    short_bools: "",
};
/// `gh issue create`. Not `--web`, `--editor`, `--recover` or `--repo`.
const ISSUE_CREATE: Flags = Flags {
    valued: &[
        "assignee",
        "body",
        "body-file",
        "label",
        "milestone",
        "project",
        "template",
        "title",
    ],
    short_valued: "abFlmpTt",
    bools: &[],
    short_bools: "",
};

/// `(option, value)` pairs (by the option's long name, or its letter), and the operands.
type GhLine<'a> = (Vec<(String, &'a str)>, Vec<&'a str>);

/// A gh command line read with `f`; why not when it has an option `f` does not list.
fn gh_parse<'a>(args: &'a [String], f: &Flags) -> Result<GhLine<'a>, String> {
    let mut opts = Vec::new();
    let mut ops = Vec::new();
    let mut i = 0;
    while let Some(x) = args.get(i) {
        i += 1;
        if let Some(long) = x.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            if f.valued.contains(&name) {
                let v = match val {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i - 1)
                            .map(String::as_str)
                            .ok_or(format!("--{name} 没有值"))?
                    }
                };
                opts.push((name.to_string(), v));
            } else if f.bools.contains(&name) && val.is_none() {
                opts.push((name.to_string(), ""));
            } else {
                return Err(format!("gh 的选项 --{name} 留给你"));
            }
        } else if let Some(short) = x.strip_prefix('-').filter(|s| !s.is_empty()) {
            for (at, c) in short.char_indices() {
                if f.short_valued.contains(c) {
                    let rest = &short[at + c.len_utf8()..];
                    let v = if rest.is_empty() {
                        i += 1;
                        args.get(i - 1)
                            .map(String::as_str)
                            .ok_or(format!("-{c} 没有值"))?
                    } else {
                        rest
                    };
                    opts.push((c.to_string(), v));
                    break;
                } else if f.short_bools.contains(c) {
                    opts.push((c.to_string(), ""));
                } else {
                    return Err(format!("gh 的选项 -{c} 留给你"));
                }
            }
        } else {
            ops.push(x.as_str());
        }
    }
    Ok((opts, ops))
}

/// `gh <args>` when it creates, edits or comments on the own branch's pull request, or creates an
/// issue: the files it reads its body from (`-F`, not `-`); otherwise why not.
pub(crate) fn gh(args: &[String], pl: &Places, own: Own) -> Result<Vec<String>, String> {
    let (a, b) = match args {
        [a, b, ..] => (a.as_str(), b.as_str()),
        _ => return Err("不是开 PR 或 issue".into()),
    };
    let flags = match (a, b) {
        ("pr", "create") => &PR_CREATE,
        ("pr", "edit") => &PR_EDIT,
        ("pr", "comment") => &PR_COMMENT,
        ("issue", "create") => &ISSUE_CREATE,
        _ => return Err(format!("gh {a} {b} 会改 GitHub")),
    };
    if !own.top {
        return Err("gh 不是直接写在这一行上的命令".into());
    }
    let (opts, ops) = gh_parse(&args[2..], flags)?;
    in_own_checkout(pl, None)?;
    let value = |long: &str, short: &str| {
        opts.iter()
            .filter(|(k, _)| k == long || k == short)
            .map(|(_, v)| *v)
            .collect::<Vec<_>>()
    };
    match (a, b) {
        ("issue", _) => {
            if let Some(x) = ops.first() {
                return Err(format!("gh issue create 不认识的参数 {x}"));
            }
        }
        ("pr", "create") => {
            if let Some(x) = ops.first() {
                return Err(format!("gh pr create 不认识的参数 {x}"));
            }
            let b = own.own()?;
            match value("head", "H").as_slice() {
                [] => own.head_is_own(b)?,
                [h] if *h == b => {}
                [h, ..] => return Err(format!("PR 的分支 {h} 不是这件事项的分支 {b}")),
            }
        }
        // edit / comment: the PR of the own branch, named or the current one
        _ => {
            let b = own.own()?;
            match ops.as_slice() {
                [] => own.head_is_own(b)?,
                [x] if *x == b => {}
                [x, ..] => {
                    return Err(format!(
                        "gh pr {sub} 的对象 {x} 不是这件事项分支 {b} 的 PR",
                        sub = args[1]
                    ));
                }
            }
        }
    }
    Ok(value("body-file", "F")
        .into_iter()
        .filter(|f| *f != "-")
        .map(String::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_like_git_the_last_value_and_the_subsection_exactly() {
        let c = Checkout::from_git(
            "steve/st-21\n",
            Some(
                "push.default\nmatching\0push.default\ncurrent\0branch.steve/st-21.pushremote\nfork\0\
                 branch.Steve/ST-21.remote\nx\0remote.origin.url\nu\0core.bare\0",
            ),
        );
        assert_eq!(c.head.as_deref(), Some("steve/st-21"));
        assert_eq!(c.get("push", None, "default"), Some("current"));
        assert_eq!(
            c.get("branch", Some("steve/st-21"), "pushRemote"),
            Some("fork")
        );
        assert_eq!(c.get("branch", Some("steve/st-21"), "remote"), None);
        assert_eq!(c.get("core", None, "bare"), Some("true"));
        assert_eq!(c.origin_rewrites(), Some(false));
        assert!(
            c.default_push("steve/st-21", true).is_err(),
            "pushRemote fork"
        );
        assert!(c.default_push("steve/st-21", false).is_ok());
        assert_eq!(Checkout::from_git("", None).head, None);
        assert_eq!(Checkout::from_git("", None).origin_rewrites(), None);
    }

    #[test]
    fn a_plain_push_goes_only_where_the_settings_send_it_to_the_own_branch() {
        let own = "steve/st-21";
        let with = |cfg: &str| Checkout::from_git(own, Some(cfg));
        for (cfg, bare, ok) in [
            ("", true, true),
            ("push.default\nsimple\0", true, true),
            ("push.default\ncurrent\0", false, true),
            ("push.default\nmatching\0", false, false),
            ("push.default\nnothing\0", true, false),
            ("push.default\nupstream\0", true, false),
            (
                "push.default\nupstream\0branch.steve/st-21.merge\nrefs/heads/steve/st-21\0",
                true,
                true,
            ),
            (
                "push.default\nupstream\0branch.steve/st-21.merge\nrefs/heads/main\0",
                true,
                false,
            ),
            ("remote.pushdefault\nfork\0", true, false),
            ("remote.pushdefault\nfork\0", false, true),
            ("branch.steve/st-21.remote\nupstream\0", true, false),
            ("branch.steve/st-21.remote\norigin\0", true, true),
        ] {
            assert_eq!(
                with(cfg).default_push(own, bare).is_ok(),
                ok,
                "{cfg:?} {bare}"
            );
        }
        assert_eq!(
            with("remote.origin.push\nrefs/heads/*:refs/heads/main\0").origin_rewrites(),
            Some(true)
        );
        assert_eq!(with("remote.origin.mirror\0").origin_rewrites(), Some(true));
        assert_eq!(
            with("remote.origin.mirror\nfalse\0").origin_rewrites(),
            Some(false)
        );
    }

    #[test]
    fn only_steve_branches_are_an_items_own() {
        for b in ["steve/st-21", "steve/0123456789ab-st-3", "steve/t1-2"] {
            assert!(is_item_branch(b), "{b}");
        }
        for b in [
            "main",
            "master",
            "HEAD",
            "steve/",
            "steve/../main",
            "steve/a b",
            "feature/x",
        ] {
            assert!(!is_item_branch(b), "{b}");
        }
    }

    #[test]
    fn a_line_keeps_head_unless_something_may_move_it() {
        for l in [
            "git push",
            "git add -A && git commit -m 'switch to x' && git push",
            "cargo test && git push origin HEAD",
            "git push -u origin HEAD && gh pr create --fill",
            "cd hub-rs && cargo build && cd .. && git push",
        ] {
            assert!(keeps_head(&shell::parse(l).unwrap()), "{l}");
        }
        for l in [
            "git switch main && git push origin HEAD",
            "git checkout -b x; git push",
            "git -C ../other checkout main; git push",
            "git bisect start && git push",
            "git stash branch x && git push",
            "gh pr checkout 3 && git push",
            "GIT_DIR=../x git status && git push",
            "export GIT_DIR=x; git push",
            "bash -c 'git switch main' && git push",
            "make && sh -c \"git checkout main\"; git push",
            "git symbolic-ref HEAD refs/heads/main; git push",
            "git branch -m other && git push",
        ] {
            assert!(!keeps_head(&shell::parse(l).unwrap()), "{l}");
        }
    }

    #[test]
    fn gh_options_are_read_as_gh_reads_them() {
        let a = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let args = a("--title=x -dB main -Fbody.md --head steve/st-21 --fill");
        let (opts, ops) = gh_parse(&args, &PR_CREATE).unwrap();
        assert!(ops.is_empty());
        assert!(opts.contains(&("title".into(), "x")));
        assert!(opts.contains(&("d".into(), "")));
        assert!(opts.contains(&("B".into(), "main")));
        assert!(opts.contains(&("F".into(), "body.md")));
        assert!(opts.contains(&("head".into(), "steve/st-21")));
        for bad in [
            "--web",
            "-w",
            "--editor",
            "-e",
            "--repo x",
            "-R x",
            "--recover f",
            "--draft=true",
            "-dw",
        ] {
            assert!(gh_parse(&a(bad), &PR_CREATE).is_err(), "{bad}");
        }
        assert!(gh_parse(&a("--delete-last"), &PR_COMMENT).is_err());
        assert!(gh_parse(&a("--yes"), &PR_COMMENT).is_err());
    }
}
