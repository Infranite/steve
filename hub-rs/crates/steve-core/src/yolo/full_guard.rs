//! Full access (完全放开) while the harness still asks: a request left over from the mode the turn
//! started in (the switch lands at the turn's end), or one its executor asks in any mode, is answered
//! as full access promises, after the hard limits (which keep `STEVE_HOME`, port 8440, launchd, other
//! items' worktrees and branches, payments, speaking as the user, pushing and publishing).
//!
//! What full access still tells the agent not to do stays with the user: root, `pkill`/`killall`, a
//! `kill` of a process it did not start. Those are judged on every command the line actually runs,
//! not only on the words it starts with: a command line runs other commands through a shell's `-c`,
//! a here-document or pipe fed to a shell or an interpreter, `eval`, `find -exec`, `xargs`, wrappers
//! (`watch`, `setsid`, `flock`…), git's `-x`/`bisect run`/aliases and inline code (`python3 -c`,
//! `node -e`…); each of those bodies is read the same way ([`Body`]). A program whose name is not
//! written out (`$CMD`, a glob, a brace list) cannot be judged, while a path or an argument behind a
//! variable is the agent's own under full access. Whatever cannot be read (a truncated input,
//! command substitution, an unparsable line, a script piped into a shell) and tools of other MCP
//! servers stay with the user: nothing can say they keep those promises.
//!
//! A quoted argument of a program that only reads it (a commit message, `echo`'s text, a pattern) is
//! data, not a command: `git commit -m 'fix (x)'` is judged as the commit it is.
//!
//! Nothing here runs anything; it only reads the text.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::json;

use super::guard::{
    Guard, READERS, SHELLS, ask, awk_parts, inline_code, kill, operands, program_from_stdin,
};
use super::judge::Tool;
use super::limits::{tool_command, tool_kind, tool_paths};
use super::shell::{self, Argv};

/// How deep commands that run commands are followed.
const MAX_DEPTH: u8 = 6;

/// Programs that run as root (or as another user).
const ROOT: &[&str] = &["sudo", "doas", "su", "pkexec", "runuser"];
/// Programs that stop processes by name or pattern, whoever started them.
const BY_NAME: &[&str] = &["pkill", "killall", "killall5", "skill", "snice"];
/// Programs that run the rest of their words, or one of their quoted words, as a command (besides
/// the wrappers [`shell::argv`] already sees through). Where the command starts depends on their
/// options, so every operand is read as a possible start.
const RUNNERS: &[&str] = &[
    "watch",
    "setsid",
    "flock",
    "script",
    "arch",
    "chroot",
    "nsenter",
    "unshare",
    "systemd-run",
    "parallel",
    "entr",
    "tmux",
    "screen",
    "coproc",
    "function",
    "trap",
    "alias",
    "npx",
    "pnpx",
    "bunx",
    "uvx",
    "strace",
    "ltrace",
    "dtruss",
    "valgrind",
    "gdb",
    "lldb",
    "hyperfine",
    "nodemon",
    "concurrently",
    "watchexec",
];
/// Programs that only read their arguments (text, patterns, files) or write files: a quoted
/// argument is data to them, never a command.
const DATA: &[&str] = &[
    "mkdir", "cp", "mv", "rm", "touch", "rmdir", "ln", "tee", "sed", "printf", "git",
];
/// Interpreters whose inline code is read as code ([`full_code`]), and the flag that gives it,
/// besides the ones [`inline_code`] knows.
const MORE_CODE: &[(&str, &[&str])] = &[
    ("php", &["-r"]),
    ("lua", &["-e"]),
    ("Rscript", &["-e"]),
    ("pwsh", &["-c", "-Command"]),
    ("powershell", &["-c", "-Command"]),
    ("osascript", &["-e"]),
];
/// Interpreters that read their program from standard input when given no code and no script.
const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "node",
    "bun",
    "tsx",
    "ts-node",
    "ruby",
    "perl",
    "deno",
    "php",
    "lua",
    "Rscript",
    "pwsh",
    "powershell",
    "osascript",
];
/// Calls in code that stop a process or change whose rights it runs with.
static PROCESS_CALLS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(kill|killpg|setuid|seteuid|setreuid|setresuid|setgid|setegid)\b")
        .expect("valid regex")
});

/// What runs inside a command, as read off its text.
enum Body {
    /// A command line a shell runs (`sh -c`, `eval`, a here-document fed to a shell): every part of
    /// it is judged, and what cannot be read in it stays with the user.
    Line(String),
    /// Text that may be a command line (a quoted word of a wrapper or of a program not known to only
    /// read it, a string in inline code): any command in it that full access keeps is found; text
    /// that does not read as a command is left as text.
    Maybe(String),
    /// Words run as a command of their own (`find -exec …`, `git bisect run …`).
    Argv(Vec<String>),
    /// Code for an interpreter.
    Code(String),
    /// Something runs that cannot be read: why.
    Unseen(String),
}

/// The answer to a tool request of a session under full access.
pub fn full(t: &Tool) -> Guard {
    if t.truncated || t.raw.is_null() {
        return ask("工具输入被截断，看不全");
    }
    let Some(option) = t
        .options
        .iter()
        .find(|(_, kind)| kind == "allow_once")
        .map(|(id, _)| id.clone())
    else {
        return ask("卡上没有「允许一次」");
    };
    let kind = tool_kind(t);
    if t.title.starts_with("mcp__") || kind == "other" {
        return ask(format!("{} 不是文件或命令操作，留给你", t.title));
    }
    let mut commands = vec![];
    if kind == "execute" {
        let Some(cmd) = tool_command(t) else {
            return ask("看不出要执行的命令");
        };
        if let Err(why) = command(&cmd, &t.own_pids) {
            return ask(why);
        }
        commands.push(cmd);
    }
    Guard::Allow {
        option,
        checks: json!({ "kind": kind, "paths": tool_paths(t), "commands": commands, "matched": "full", "level": "full" }),
    }
}

/// Every command `cmd` runs keeps full access's promises; or why not.
pub(crate) fn command(cmd: &str, pids: &[u32]) -> Result<(), String> {
    let p = shell::parse(cmd).map_err(|e| format!("命令看不懂：{}", e.0))?;
    parsed(&p, pids, 0, true)
}

/// `strict`: a command line that runs as written, where anything unreadable stays with the user;
/// otherwise text that may be one ([`Body::Maybe`]), where only the commands found are judged.
fn parsed(p: &shell::Parsed, pids: &[u32], depth: u8, strict: bool) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("命令嵌套太深，看不清".into());
    }
    if strict && let Some(what) = p.opaque.first() {
        return Err(format!("命令里有 {what}，看不出实际会执行什么"));
    }
    for c in &p.cmds {
        // `env -S 'a b'` splits its value into the command
        if let Some(i) = c.words.iter().position(|w| shell::basename(w) == "env")
            && let Some(v) = env_split(&c.words[i + 1..])
        {
            body(Body::Line(v), pids, depth)?;
        }
        let Some(a) = shell::argv(&c.words) else {
            continue;
        };
        let fed = Fed {
            stdin: c.stdin.as_deref(),
            piped: c.piped,
        };
        argv(&a, fed, pids, depth)?;
    }
    Ok(())
}

/// What a command's standard input is.
#[derive(Clone, Copy, Default)]
struct Fed<'a> {
    /// A here-document.
    stdin: Option<&'a str>,
    /// The output of the command before it.
    piped: bool,
}

/// One command: what it is, and every command it runs in turn.
fn argv(a: &Argv, fed: Fed, pids: &[u32], depth: u8) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("命令嵌套太深，看不清".into());
    }
    let p = a.program.as_str();
    if dynamic(p) {
        return Err(format!(
            "要运行的程序名不是写明的（{p}），看不出实际会运行什么"
        ));
    }
    if a.sudo || ROOT.contains(&p) {
        return Err("以 root 或别的用户身份运行，留给你".into());
    }
    if BY_NAME.contains(&p) {
        return Err(format!("{p} 留给你：只按 PID 停自己启动的进程"));
    }
    if p == "kill" {
        if a.xargs {
            return Err("kill 的 PID 来自标准输入，看不出是不是自己启动的进程".into());
        }
        kill(&a.args, pids)?;
    }
    if p == "fuser" && a.args.iter().any(|x| x.starts_with('-') && x.contains('k')) {
        return Err("fuser -k 会停掉用到这个文件的所有进程，留给你".into());
    }
    for b in bodies(a, fed) {
        body(b, pids, depth + 1)?;
    }
    Ok(())
}

fn body(b: Body, pids: &[u32], depth: u8) -> Result<(), String> {
    match b {
        Body::Line(s) => {
            let p = shell::parse(&s).map_err(|e| format!("嵌套的命令看不懂：{}", e.0))?;
            parsed(&p, pids, depth, true)
        }
        Body::Maybe(s) => match shell::parse(&s) {
            Ok(p) => parsed(&p, pids, depth, false),
            Err(_) => Ok(()),
        },
        Body::Argv(words) => match shell::argv(&words) {
            Some(a) => argv(&a, Fed::default(), pids, depth),
            None => Ok(()),
        },
        Body::Code(code) => self::code(&code, pids, depth),
        Body::Unseen(why) => Err(why),
    }
}

/// A program name that is not written out: a variable (`$CMD`, `"${X:-pkill}"`), a glob or a brace
/// list. Its directory may be a variable (`$HOME/bin/tool`), its name may not.
fn dynamic(p: &str) -> bool {
    // `[` and `[[` are the test command, not a glob
    !matches!(p, "[" | "[[") && (p.is_empty() || p.contains(['$', '*', '?', '[', '{', '}']))
}

/// Inline code (or code fed on standard input): the calls that stop processes or change rights, and
/// every string in it read as a possible command (`os.system("…")`, `execSync('…')`, `["sudo", …]`).
fn code(code: &str, pids: &[u32], depth: u8) -> Result<(), String> {
    if let Some(m) = PROCESS_CALLS.find(code) {
        return Err(format!(
            "代码里有结束进程或改变运行身份的调用（{}），看不出动的是谁的进程",
            m.as_str()
        ));
    }
    for s in strings(code) {
        body(Body::Maybe(s), pids, depth + 1)?;
    }
    Ok(())
}

/// The quoted strings in `code` ('…', "…", `…`), escapes read, unclosed ones to the end.
fn strings(code: &str) -> Vec<String> {
    let mut out = vec![];
    let mut it = code.chars();
    while let Some(c) = it.next() {
        if !matches!(c, '\'' | '"' | '`') {
            continue;
        }
        let mut s = String::new();
        while let Some(d) = it.next() {
            match d {
                '\\' => s.extend(it.next()),
                _ if d == c => break,
                _ => s.push(d),
            }
        }
        out.push(s);
    }
    out
}

/// The value of `env -S`/`--split-string` among `env`'s options.
fn env_split(words: &[String]) -> Option<String> {
    let mut it = words.iter();
    while let Some(w) = it.next() {
        if w == "-S" || w == "--split-string" {
            return it.next().cloned();
        }
        if let Some(v) = w.strip_prefix("--split-string=") {
            return Some(v.into());
        }
        if !w.starts_with('-') {
            return None;
        }
    }
    None
}

/// What `a` runs besides itself.
fn bodies(a: &Argv, fed: Fed) -> Vec<Body> {
    let p = a.program.as_str();
    let args = &a.args;
    let mut out = vec![];
    // a script fed on standard input
    let from_stdin = |out: &mut Vec<Body>, code: bool| {
        if let Some(s) = fed.stdin {
            out.push(if code {
                Body::Code(s.into())
            } else {
                Body::Line(s.into())
            });
        } else if fed.piped {
            out.push(Body::Unseen(format!(
                "管道把脚本喂给 {p}，看不出会执行什么"
            )));
        }
    };
    if SHELLS.contains(&p) {
        match args.iter().position(|x| shell::is_dash_c(x)) {
            Some(i) => match args.get(i + 1) {
                Some(s) => out.push(Body::Line(s.clone())),
                None => out.push(Body::Unseen(format!(
                    "{p} -c 的脚本{}，看不出会执行什么",
                    if a.xargs {
                        "来自标准输入"
                    } else {
                        "缺失"
                    }
                ))),
            },
            // `bash script.sh` runs a file, as any program would; with no script it reads stdin
            None if operands(args).iter().all(|x| x.as_str() == "-") => from_stdin(&mut out, false),
            None => {}
        }
        return out;
    }
    match p {
        "eval" => {
            if a.xargs {
                out.push(Body::Unseen("eval 的内容来自标准输入".into()));
            } else {
                out.push(Body::Line(args.join(" ")));
            }
            return out;
        }
        "source" | "." => {
            if operands(args)
                .first()
                .is_none_or(|f| matches!(f.as_str(), "/dev/stdin" | "-" | "/dev/fd/0"))
            {
                from_stdin(&mut out, false);
            }
            return out;
        }
        "find" => {
            let mut i = 0;
            while i < args.len() {
                if matches!(args[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                    let end = args[i + 1..]
                        .iter()
                        .position(|x| x == ";" || x == "+")
                        .map_or(args.len(), |e| i + 1 + e);
                    out.push(Body::Argv(args[i + 1..end].to_vec()));
                    i = end;
                }
                i += 1;
            }
            return out;
        }
        "git" => return git(args),
        "awk" | "gawk" | "mawk" | "nawk" => {
            if let Some((progs, _)) = awk_parts(args) {
                out.extend(progs.into_iter().map(|s| Body::Code(s.into())));
            }
            return out;
        }
        _ => {}
    }
    if INTERPRETERS.contains(&p) {
        let more = MORE_CODE
            .iter()
            .find(|(n, _)| *n == p)
            .and_then(|(_, flags)| {
                let i = args.iter().position(|x| flags.contains(&x.as_str()))?;
                args.get(i + 1).map(String::as_str)
            });
        let reads_stdin = if MORE_CODE.iter().any(|(n, _)| *n == p) {
            operands(args).is_empty()
        } else {
            program_from_stdin(p, args)
        };
        match inline_code(p, args).or(more) {
            Some(c) => out.push(Body::Code(c.into())),
            None if reads_stdin => from_stdin(&mut out, true),
            None => {}
        }
        return out;
    }
    if RUNNERS.contains(&p) {
        // the command may start at any operand (`flock /tmp/l sudo x`, `watch -n 1 pkill x`)
        for (i, x) in args.iter().enumerate() {
            if !x.starts_with('-') {
                out.push(Body::Argv(args[i..].to_vec()));
                out.push(Body::Maybe(
                    x.split_once('=').map_or(x.as_str(), |(_, v)| v).to_string(),
                ));
            }
        }
    } else if !DATA.contains(&p) && !READERS.contains(&p) {
        // a program not known to only read its words: a quoted word may be a command it runs
        out.extend(
            args.iter()
                .filter(|x| x.contains([' ', ';', '|', '&', '\n']))
                .map(|x| Body::Maybe(x.clone())),
        );
        // and what it is fed on standard input may be a script
        if let Some(s) = fed.stdin {
            out.push(Body::Maybe(s.into()));
        }
    }
    out
}

/// What git runs besides itself: `-c` values that are commands (an alias `!…`, a pager, an
/// editor), `rebase -x`, `bisect run`, `submodule foreach`. Its other words (a commit message, a
/// path, a pattern) are data.
fn git(args: &[String]) -> Vec<Body> {
    let mut out = vec![];
    let mut i = 0;
    while let Some(x) = args.get(i) {
        match x.as_str() {
            "-c" => {
                if let Some((_, v)) = args.get(i + 1).and_then(|kv| kv.split_once('=')) {
                    out.push(Body::Maybe(v.trim_start_matches('!').to_string()));
                }
                i += 2;
            }
            "-C" => i += 2,
            s if s.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let sub = args.get(i).map(String::as_str).unwrap_or("");
    let rest = &args[(i + 1).min(args.len())..];
    match sub {
        "rebase" => {
            let mut it = rest.iter();
            while let Some(x) = it.next() {
                if x == "-x" || x == "--exec" {
                    out.extend(it.next().map(|c| Body::Line(c.clone())));
                } else if let Some(c) = x.strip_prefix("--exec=") {
                    out.push(Body::Line(c.into()));
                }
            }
        }
        "bisect" if rest.first().is_some_and(|x| x == "run") => {
            out.push(Body::Argv(rest[1..].to_vec()));
        }
        "submodule" if rest.iter().any(|x| x == "foreach") => {
            let j = rest.iter().position(|x| x == "foreach").unwrap_or(0);
            let cmd: Vec<&String> = rest[j + 1..]
                .iter()
                .skip_while(|x| x.starts_with('-'))
                .collect();
            out.push(Body::Line(
                cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "),
            ));
        }
        _ => {}
    }
    out
}
