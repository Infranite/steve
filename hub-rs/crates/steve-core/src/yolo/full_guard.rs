//! Full access (完全放开) while the harness still asks: a request left over from the mode the turn
//! started in (the switch lands at the turn's end), or one its executor asks in any mode, is answered
//! as full access promises.
//!
//! Every command the line runs is judged the same way, not only the words it starts with: the bodies
//! [`super::exec`] reads off the text (a shell's `-c`, a here-document or pipe fed to a shell or an
//! interpreter, `eval`, `find -exec`, `xargs`, wrappers such as `env -S`, `watch`, `setsid`,
//! `flock`, git's `-x`/`bisect run`/aliases, inline code), each with what its standard input is.
//! On each command:
//!
//! - the hard limits ([`limits::simple`]: `STEVE_HOME`, port 8440, launchd, other items' worktrees
//!   and branches, payments, speaking as the user, pushing and publishing), as on the line itself;
//! - what full access still tells the agent not to do: root (also `sudo -i`/`sudo -s`, a root shell
//!   with no command), `pkill`/`killall`, a `kill` of a process it did not start.
//!
//! What cannot be read stays with the user, never guessed: a program whose name is not written out
//! (`$CMD`, a glob, a brace list; a path or an argument behind a variable is the agent's own), a
//! wrapper option not known (so where its command starts is not known), a script piped into a shell
//! or an interpreter, inline code that can start programs or evaluate text ([`exec::code_runs`]), a
//! sed script that executes, command substitution, an unparsable line, a truncated input and tools of
//! other MCP servers. A program or script in a file runs as the program it is: full access lets
//! programs the agent wrote run, and what they do inside is not read here.
//!
//! A quoted argument of a program that only reads it (a commit message, `echo`'s text, a pattern) is
//! data, not a command: `git commit -m 'fix (x)'` is judged as the commit it is.
//!
//! Nothing here runs anything; it only reads the text.

use serde_json::json;

use super::exec::{self, Body, Feed};
use super::guard::{Guard, ask, kill};
use super::judge::{Env, Tool};
use super::limits::{self, Limit, Places, tool_base, tool_command, tool_kind, tool_paths};
use super::shell::{self, Argv, Simple};

/// How deep commands that run commands are followed.
const MAX_DEPTH: u8 = 6;

/// Programs that run as root (or as another user).
const ROOT: &[&str] = &["sudo", "doas", "su", "pkexec", "runuser", "sudoedit"];
/// Programs that stop processes by name or pattern, whoever started them.
const BY_NAME: &[&str] = &["pkill", "killall", "killall5", "skill", "snice"];

/// The answer to a tool request of a session under full access; `Err`: a hard limit one of the
/// commands it runs hits.
pub fn full(t: &Tool, env: &Env) -> Result<Guard, Limit> {
    if t.truncated || t.raw.is_null() {
        return Ok(ask("工具输入被截断，看不全"));
    }
    let Some(option) = t
        .options
        .iter()
        .find(|(_, kind)| kind == "allow_once")
        .map(|(id, _)| id.clone())
    else {
        return Ok(ask("卡上没有「允许一次」"));
    };
    let kind = tool_kind(t);
    if t.title.starts_with("mcp__") || kind == "other" {
        return Ok(ask(format!("{} 不是文件或命令操作，留给你", t.title)));
    }
    let mut commands = vec![];
    if kind == "execute" {
        let Some(cmd) = tool_command(t) else {
            return Ok(ask("看不出要执行的命令"));
        };
        let mut pl = Places::new(t.cwd.as_deref(), env);
        pl.full = true;
        if let Some(base) = tool_base(t, &pl) {
            pl.base = base;
        }
        let walk = Walk {
            branch: t.branch.as_deref(),
            pids: &t.own_pids,
        };
        match walk.line(&cmd, &Feed::default(), &pl, 0, true) {
            Ok(()) => {}
            Err(Stop::Limit(l)) => return Err(l),
            Err(Stop::Ask(why)) => return Ok(ask(why)),
        }
        commands.push(cmd);
    }
    Ok(Guard::Allow {
        option,
        checks: json!({ "kind": kind, "paths": tool_paths(t), "commands": commands, "matched": "full", "level": "full" }),
    })
}

/// Why a command line is not answered.
enum Stop {
    /// A hard limit.
    Limit(Limit),
    /// What full access keeps, or what cannot be read.
    Ask(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Self {
        Stop::Ask(why)
    }
}

/// Follows every body of a command line.
struct Walk<'a> {
    /// The item's own branch (deleting it is not another item's).
    branch: Option<&'a str>,
    /// Processes the session's agent started.
    pids: &'a [u32],
}

impl Walk<'_> {
    /// A command line, fed `feed`, run where `pl` says. `strict`: it runs as written, and anything
    /// unreadable stays with the user; otherwise it is text that may be a command line
    /// ([`Body::Maybe`]), where only the commands found are judged.
    fn line(
        &self,
        src: &str,
        feed: &Feed,
        pl: &Places,
        depth: u8,
        strict: bool,
    ) -> Result<(), Stop> {
        if depth > MAX_DEPTH {
            return Err(Stop::Ask("命令嵌套太深，看不清".into()));
        }
        let p = match shell::parse(src) {
            Ok(p) => p,
            Err(e) if strict => return Err(Stop::Ask(format!("命令看不懂：{}", e.0))),
            Err(_) => return Ok(()),
        };
        if strict && let Some(what) = p.opaque.first() {
            return Err(Stop::Ask(format!("命令里有 {what}，看不出实际会执行什么")));
        }
        let mut pl = pl.clone();
        for c in &p.cmds {
            if pl.cd(&c.words) {
                continue;
            }
            // in text that may be a command line, words that cannot name a program (a pattern
            // such as `^TestA$`, a query such as `.a[]`) are text
            if !strict && shell::argv(&c.words).is_some_and(|a| dynamic(&a.program) && !a.sudo) {
                continue;
            }
            self.simple(c, &Feed::of(c, feed), &pl, depth)?;
        }
        Ok(())
    }

    /// One simple command: the hard limits as written, then what its variables, it and what it
    /// runs in turn do.
    fn simple(&self, c: &Simple, feed: &Feed, pl: &Places, depth: u8) -> Result<(), Stop> {
        if let Some(l) = limits::simple(c, pl, self.branch, 0) {
            return Err(Stop::Limit(l));
        }
        for b in exec::var_bodies(&c.words) {
            self.body(b, pl, depth)?;
        }
        match shell::argv(&c.words) {
            Some(a) => self.argv(&a, feed, pl, depth),
            None => Ok(()),
        }
    }

    fn argv(&self, a: &Argv, feed: &Feed, pl: &Places, depth: u8) -> Result<(), Stop> {
        let p = a.program.as_str();
        if dynamic(p) {
            return Err(Stop::Ask(format!(
                "要运行的程序名不是写明的（{p}），看不出实际会运行什么"
            )));
        }
        if a.sudo || ROOT.contains(&p) {
            return Err(Stop::Ask("以 root 或别的用户身份运行，留给你".into()));
        }
        if BY_NAME.contains(&p) {
            return Err(Stop::Ask(format!("{p} 留给你：只按 PID 停自己启动的进程")));
        }
        if p == "kill" {
            if a.xargs {
                return Err(Stop::Ask(
                    "kill 的 PID 来自标准输入，看不出是不是自己启动的进程".into(),
                ));
            }
            kill(&a.args, self.pids)?;
        }
        if p == "fuser" && a.args.iter().any(|x| x.starts_with('-') && x.contains('k')) {
            return Err(Stop::Ask(
                "fuser -k 会停掉用到这个文件的所有进程，留给你".into(),
            ));
        }
        for b in exec::bodies(a, feed) {
            self.body(b, pl, depth)?;
        }
        Ok(())
    }

    /// One thing a command runs, one level below it.
    fn body(&self, b: Body, pl: &Places, depth: u8) -> Result<(), Stop> {
        match b {
            Body::Line(s, f) => self.line(&s, &f, pl, depth + 1, true),
            Body::Maybe(s) => self.line(&s, &Feed::default(), pl, depth + 1, false),
            Body::Argv(words, f) => {
                if depth >= MAX_DEPTH {
                    return Err(Stop::Ask("命令嵌套太深，看不清".into()));
                }
                let c = Simple {
                    expands: words.iter().any(|w| w.contains('$')),
                    words,
                    ..Default::default()
                };
                self.simple(&c, &f, pl, depth + 1)
            }
            Body::Code(lang, code) => match exec::code_runs(lang, &code) {
                Some(why) => Err(Stop::Ask(why)),
                None => Ok(()),
            },
            Body::Unseen(why) => Err(Stop::Ask(why)),
        }
    }
}

/// A program name that is not written out: a variable (`$CMD`, `"${X:-pkill}"`), a glob or a brace
/// list. Its directory may be a variable (`$HOME/bin/tool`), its name may not.
fn dynamic(p: &str) -> bool {
    // `[` and `[[` are the test command, not a glob
    !matches!(p, "[" | "[[") && (p.is_empty() || p.contains(['$', '*', '?', '[', '{', '}']))
}
