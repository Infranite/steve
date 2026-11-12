//! What a command runs besides itself, as read off its text: the bodies full access judges one by
//! one ([`super::full_guard`]), each with what its standard input is. Nothing here runs anything.
//!
//! A body is a command line a shell runs (`sh -c`, `eval`, a here-document fed to a shell, `watch`'s
//! words), words run as a command (`find -exec`, `setsid`, `flock`), code for an interpreter, or
//! something that runs but cannot be read here. Wrappers are read by their documented options
//! ([`shell::Wrapper`]); an option not listed leaves the start of their command unknown, and it is
//! never guessed from the operands.
//!
//! What a static reading cannot do, and so leaves to the user instead of guessing: a script piped
//! into a shell or an interpreter, inline code that can start programs, evaluate text or reach a
//! function by a computed name ([`code_runs`]), a sed script that executes (`e`, `s///e`) or sits in
//! a file, and programs that run commands in ways not read here ([`OPAQUE`]). A program or script in
//! a file (`./build.sh`, `python3 tool.py`, `make`) is read as the program it is, not followed.

use std::sync::LazyLock;

use regex::Regex;

use super::guard::{READERS, SHELLS, awk_parts, operands};
use super::shell::{self, Argv, HELP, NO_LONG, Simple, Wrapper};

/// A command's standard input, as far as the text tells.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Feed {
    /// A here-document.
    pub stdin: Option<String>,
    /// The output of the command before it.
    pub piped: bool,
}

impl Feed {
    /// The input of command `c` of a line that is itself fed `outer`: its own here-document, the
    /// pipe before it, or what the line was fed (`env -S bash`, `sh -c 'bash'` pass theirs on).
    pub fn of(c: &Simple, outer: &Feed) -> Feed {
        match &c.stdin {
            Some(h) => Feed {
                stdin: Some(h.clone()),
                piped: false,
            },
            None if c.piped => Feed {
                stdin: None,
                piped: true,
            },
            None => outer.clone(),
        }
    }
}

/// The languages whose inline code is read ([`code_runs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Python,
    Js,
    Ruby,
    Perl,
    Php,
    Lua,
    R,
    Awk,
    /// A shell of its own (PowerShell): its commands are not read here.
    Shell,
    AppleScript,
}

/// What runs inside a command.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Body {
    /// A command line a shell runs, fed this input: every part of it is judged, and what cannot be
    /// read in it stays with the user.
    Line(String, Feed),
    /// Text that may be a command line (a quoted word of a program not known to only read its
    /// words): the commands found in it are judged; text that does not read as one is left as text.
    Maybe(String),
    /// Words run as a command of their own, fed this input.
    Argv(Vec<String>, Feed),
    /// Code for an interpreter.
    Code(Lang, String),
    /// Something runs that cannot be read here: why.
    Unseen(String),
}

/// Programs that only read their arguments (text, patterns, files) or write files: a quoted
/// argument is data to them, never a command. (`sed`'s scripts are read by [`sed_runs`].)
const DATA: &[&str] = &[
    "mkdir", "cp", "mv", "rm", "touch", "rmdir", "ln", "tee", "printf", "git", "command",
];

/// Programs that run commands in ways not read here (a terminal multiplexer's windows, a debugger's
/// or tracer's own commands, a transient service, another namespace or root, a benchmark or job
/// runner's templates): their command stays with the user.
pub(crate) const OPAQUE: &[&str] = &[
    "tmux",
    "screen",
    "systemd-run",
    "nsenter",
    "unshare",
    "chroot",
    "parallel",
    "strace",
    "ltrace",
    "dtruss",
    "gdb",
    "lldb",
    "hyperfine",
    "nodemon",
    "concurrently",
    "watchexec",
    "expect",
    "sshpass",
];

/// Runners whose command starts after their options: `watch`'s words are a line for `sh -c` (with
/// `-x`, a command of their own); `flock` takes its lock file first, then a command or `-c LINE`;
/// `script` its log file; `entr -s` a line, else a command.
const RUNNERS: &[Wrapper] = &[
    Wrapper {
        names: &["watch"],
        flags: "bcCdegprtwxhv",
        valued: "nqs",
        attached: "",
        long_flags: &[
            "--beep",
            "--color",
            "--no-color",
            "--differences",
            "--errexit",
            "--chgexit",
            "--precise",
            "--no-rerun",
            "--no-title",
            "--no-wrap",
            "--no-linewrap",
            "--exec",
            "--help",
            "--version",
        ],
        long_valued: &["--interval", "--equexit", "--shotsdir"],
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["setsid"],
        flags: "cfw",
        valued: "",
        attached: "",
        long_flags: &["--ctty", "--fork", "--wait", "--help", "--version"],
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["flock"],
        flags: "sexnouFhV",
        valued: "wE",
        attached: "",
        long_flags: &[
            "--shared",
            "--exclusive",
            "--unlock",
            "--nb",
            "--nonblock",
            "--close",
            "--no-fork",
            "--verbose",
            "--help",
            "--version",
        ],
        long_valued: &["--wait", "--timeout", "--conflict-exit-code"],
        numeric: false,
        lead: 1,
    },
    Wrapper {
        names: &["script"],
        flags: "adeFfkpqr",
        valued: "cEBIOTmo",
        attached: "",
        long_flags: &[
            "--append",
            "--return",
            "--flush",
            "--force",
            "--quiet",
            "--help",
            "--version",
        ],
        long_valued: &[
            "--command",
            "--echo",
            "--log-in",
            "--log-out",
            "--log-io",
            "--log-timing",
            "--logging-format",
            "--output-limit",
        ],
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["entr"],
        flags: "acdnprsz",
        valued: "",
        attached: "",
        long_flags: NO_LONG,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["npx", "pnpx", "bunx"],
        flags: "yq",
        valued: "pcw",
        attached: "",
        long_flags: &[
            "--yes",
            "--no",
            "--quiet",
            "--silent",
            "--verbose",
            "--ignore-existing",
            "--no-install",
            "--prefer-offline",
            "--prefer-online",
            "--offline",
            "--legacy-peer-deps",
            "--workspaces",
            "--include-workspace-root",
            "--bun",
            "--help",
            "--version",
        ],
        long_valued: &[
            "--package",
            "--call",
            "--prefix",
            "--registry",
            "--cache",
            "--userconfig",
            "--workspace",
            "--shell",
            "--node-options",
            "--loglevel",
        ],
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["valgrind"],
        flags: "vqh",
        valued: "",
        attached: "",
        long_flags: HELP,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
];

/// What `a` runs besides itself, `a` being fed `feed`.
pub(crate) fn bodies(a: &Argv, feed: &Feed) -> Vec<Body> {
    let p = a.program.as_str();
    let args = &a.args;
    let mut out = vec![];
    // a script fed on standard input: a here-document is read, a pipe is not
    let from_stdin = |out: &mut Vec<Body>, lang: Option<Lang>| {
        if let Some(s) = &feed.stdin {
            out.push(match lang {
                Some(l) => Body::Code(l, s.clone()),
                None => Body::Line(s.clone(), Feed::default()),
            });
        } else if feed.piped {
            out.push(Body::Unseen(format!(
                "管道把脚本喂给 {p}，看不出会执行什么"
            )));
        }
    };
    if let Some(why) = &a.unclear {
        out.push(Body::Unseen(format!("{why}，看不出它运行的命令从哪开始")));
        return out;
    }
    if SHELLS.contains(&p) {
        match args.iter().position(|x| shell::is_dash_c(x)) {
            Some(i) => match args.get(i + 1) {
                Some(s) => out.push(Body::Line(s.clone(), feed.clone())),
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
            None if operands(args).iter().all(|x| x.as_str() == "-") => from_stdin(&mut out, None),
            None => {}
        }
        return out;
    }
    if OPAQUE.contains(&p) {
        out.push(Body::Unseen(format!(
            "{p} 运行命令的方式这里读不出来，留给你"
        )));
        return out;
    }
    match p {
        "eval" => {
            out.push(if a.xargs {
                Body::Unseen("eval 的内容来自标准输入".into())
            } else {
                Body::Line(args.join(" "), feed.clone())
            });
            return out;
        }
        "source" | "." => {
            if operands(args)
                .first()
                .is_none_or(|f| matches!(f.as_str(), "/dev/stdin" | "-" | "/dev/fd/0"))
            {
                from_stdin(&mut out, None);
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
                    out.push(Body::Argv(args[i + 1..end].to_vec(), feed.clone()));
                    i = end;
                }
                i += 1;
            }
            return out;
        }
        "git" => return git(args, feed),
        // GNU sed is `gsed` where the system's is BSD's
        "sed" | "gsed" => {
            if let Some(why) = sed_runs(args) {
                out.push(Body::Unseen(why));
            }
            return out;
        }
        "awk" | "gawk" | "mawk" | "nawk" => {
            match awk_parts(args) {
                Some((progs, _)) => {
                    out.extend(progs.into_iter().map(|s| Body::Code(Lang::Awk, s.into())));
                }
                None => out.push(Body::Unseen("awk 的程序在文件里，看不出会做什么".into())),
            }
            return out;
        }
        "alias" => {
            // `alias k=pkill` makes `k` run that line
            out.extend(
                operands(args)
                    .into_iter()
                    .filter_map(|x| x.split_once('='))
                    .map(|(_, v)| Body::Line(v.to_string(), feed.clone())),
            );
            return out;
        }
        "trap" => {
            if let Some(action) = operands(args).first()
                && !matches!(action.as_str(), "" | "-")
                && !args.iter().any(|x| x == "-l" || x == "-p")
            {
                out.push(Body::Line(action.to_string(), feed.clone()));
            }
            return out;
        }
        "function" => {
            // `function f { …`: what follows the brace is a command of its own
            if let Some(i) = args.iter().position(|x| x == "{") {
                out.push(Body::Argv(args[i + 1..].to_vec(), feed.clone()));
            }
            return out;
        }
        "coproc" => {
            out.push(Body::Argv(args.clone(), feed.clone()));
            return out;
        }
        "arch" => {
            // its options are architecture names (`-arm64`); `-arch`, `-d` and `-e` take a value
            let mut i = 0;
            while let Some(x) = args.get(i) {
                match x.as_str() {
                    "--" => {
                        i += 1;
                        break;
                    }
                    "-arch" | "-d" | "-e" => i += 2,
                    s if s.starts_with('-') => i += 1,
                    _ => break,
                }
            }
            out.push(Body::Argv(
                args.get(i..).map(<[String]>::to_vec).unwrap_or_default(),
                feed.clone(),
            ));
            return out;
        }
        _ => {}
    }
    if let Some(spec) = RUNNERS.iter().find(|s| s.names.contains(&p)) {
        runner(p, spec, args, feed, &mut out);
        return out;
    }
    if let Some(it) = interpreter(p, args) {
        match it.lang {
            Lang::Shell if !it.codes.is_empty() || it.stdin => out.push(Body::Unseen(format!(
                "{p} 是另一种 shell，它的命令这里读不出来"
            ))),
            _ => {
                out.extend(it.codes.into_iter().map(|c| Body::Code(it.lang, c)));
                out.extend(it.lines.into_iter().map(|l| Body::Line(l, feed.clone())));
                if it.stdin {
                    from_stdin(&mut out, Some(it.lang));
                }
            }
        }
        return out;
    }
    if !DATA.contains(&p) && !READERS.contains(&p) {
        // a program not known to only read its words: a quoted word may be a command it runs
        out.extend(
            args.iter()
                .filter(|x| x.contains([' ', ';', '|', '&', '\n']))
                .map(|x| Body::Maybe(x.clone())),
        );
        // and what it is fed on standard input may be a script
        if let Some(s) = &feed.stdin {
            out.push(Body::Maybe(s.clone()));
        }
    }
    out
}

/// The command of a runner ([`RUNNERS`]) after its options.
fn runner(p: &str, spec: &Wrapper, args: &[String], feed: &Feed, out: &mut Vec<Body>) {
    let (start, opts) = match shell::options(spec, args, 0) {
        Ok(x) => x,
        Err(why) => {
            out.push(Body::Unseen(format!("{why}，看不出它运行的命令从哪开始")));
            return;
        }
    };
    let has = |names: &[&str]| opts.iter().any(|(o, _)| names.contains(&o.as_str()));
    let value = |names: &[&str]| {
        opts.iter()
            .find(|(o, _)| names.contains(&o.as_str()))
            .and_then(|(_, v)| v.clone())
    };
    let rest = |from: usize| args.get(from..).map(<[String]>::to_vec).unwrap_or_default();
    let words = rest(start);
    match p {
        // without -x, watch hands its words, joined, to `sh -c`
        "watch" if !has(&["-x", "--exec"]) => {
            if !words.is_empty() {
                out.push(Body::Line(words.join(" "), feed.clone()));
            }
        }
        // `flock FILE -c LINE` or `flock FILE CMD…`
        "flock" => match args.get(start + 1).map(String::as_str) {
            Some("-c" | "--command") => match args.get(start + 2) {
                Some(l) => out.push(Body::Line(l.clone(), feed.clone())),
                None => out.push(Body::Unseen("flock -c 后面没有命令".into())),
            },
            Some(_) => out.push(Body::Argv(rest(start + 1), feed.clone())),
            None => {}
        },
        // `script [FILE [CMD…]]`, or `script -c LINE [FILE]`
        "script" => {
            if let Some(l) = value(&["-c", "--command"]) {
                out.push(Body::Line(l, feed.clone()));
            }
            if words.len() > 1 {
                out.push(Body::Argv(words[1..].to_vec(), feed.clone()));
            }
        }
        "entr" if has(&["-s"]) => {
            if let Some(l) = words.first() {
                out.push(Body::Line(l.clone(), Feed::default()));
            }
        }
        _ => {
            if let Some(l) = value(&["-c", "--call"]) {
                out.push(Body::Line(l, feed.clone()));
            }
            if !words.is_empty() {
                out.push(Body::Argv(words, feed.clone()));
            }
        }
    }
}

/// What git runs besides itself: `-c` values that are commands (an alias `!…`, a pager, an
/// editor), `rebase -x`, `bisect run`, `submodule foreach`. Its other words (a commit message, a
/// path, a pattern) are data.
fn git(args: &[String], feed: &Feed) -> Vec<Body> {
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
                    out.extend(it.next().map(|c| Body::Line(c.clone(), feed.clone())));
                } else if let Some(c) = x.strip_prefix("--exec=") {
                    out.push(Body::Line(c.into(), feed.clone()));
                }
            }
        }
        "bisect" if rest.first().is_some_and(|x| x == "run") => {
            out.push(Body::Argv(rest[1..].to_vec(), feed.clone()));
        }
        "submodule" if rest.iter().any(|x| x == "foreach") => {
            let j = rest.iter().position(|x| x == "foreach").unwrap_or(0);
            let cmd: Vec<&str> = rest[j + 1..]
                .iter()
                .map(String::as_str)
                .skip_while(|x| x.starts_with('-'))
                .collect();
            out.push(Body::Line(cmd.join(" "), feed.clone()));
        }
        _ => {}
    }
    out
}

// ── interpreters ──

/// How an interpreter takes code on its command line.
struct Interp {
    names: &'static [&'static str],
    lang: Lang,
    /// Short options whose value is code (`-c`, `-e`).
    code: &'static str,
    /// …and whether that value may be attached (`-c'…'`, `-ne'…'`); otherwise it is the next word.
    code_attached: bool,
    /// Long options whose value is code (`--eval`, `--print`).
    long_code: &'static [&'static str],
    /// Short options with a value (attached or the next word) that is not code.
    valued: &'static str,
    long_valued: &'static [&'static str],
    /// Short options whose value can only be attached (perl's `-i.bak`, `-l0`).
    attached: &'static str,
}

const INTERPRETERS: &[Interp] = &[
    Interp {
        names: &["python", "python3", "py", "pypy", "pypy3"],
        lang: Lang::Python,
        code: "c",
        code_attached: true,
        long_code: NO_LONG,
        valued: "WXmQ",
        long_valued: &["--check-hash-based-pycs"],
        attached: "",
    },
    Interp {
        names: &["node", "nodejs", "tsx", "ts-node"],
        lang: Lang::Js,
        code: "ep",
        code_attached: false,
        long_code: &["--eval", "--print"],
        valued: "rC",
        long_valued: &[
            "--require",
            "--import",
            "--conditions",
            "--loader",
            "--experimental-loader",
            "--input-type",
            "--env-file",
            "--title",
        ],
        attached: "",
    },
    Interp {
        names: &["bun"],
        lang: Lang::Js,
        code: "ep",
        code_attached: false,
        long_code: &["--eval", "--print"],
        valued: "r",
        long_valued: &["--preload", "--cwd", "--env-file", "--config"],
        attached: "",
    },
    Interp {
        names: &["deno"],
        lang: Lang::Js,
        code: "",
        code_attached: false,
        long_code: &["--eval"],
        valued: "",
        long_valued: &["--config", "--import-map", "--location", "--env-file"],
        attached: "",
    },
    Interp {
        names: &["ruby", "irb"],
        lang: Lang::Ruby,
        code: "e",
        code_attached: true,
        long_code: NO_LONG,
        valued: "IrCEF",
        long_valued: &["--encoding", "--external-encoding", "--internal-encoding"],
        attached: "ix0lWT",
    },
    Interp {
        names: &["perl"],
        lang: Lang::Perl,
        code: "eE",
        code_attached: true,
        long_code: NO_LONG,
        valued: "IMmF",
        long_valued: NO_LONG,
        attached: "il0xCdD",
    },
    Interp {
        names: &["php"],
        lang: Lang::Php,
        code: "rBRE",
        code_attached: true,
        long_code: NO_LONG,
        valued: "cdfzt",
        long_valued: NO_LONG,
        attached: "",
    },
    Interp {
        names: &["lua", "luajit"],
        lang: Lang::Lua,
        code: "e",
        code_attached: true,
        long_code: NO_LONG,
        valued: "l",
        long_valued: NO_LONG,
        attached: "",
    },
    Interp {
        names: &["Rscript", "R"],
        lang: Lang::R,
        code: "e",
        code_attached: false,
        long_code: NO_LONG,
        valued: "",
        long_valued: NO_LONG,
        attached: "",
    },
    Interp {
        names: &["pwsh", "powershell"],
        lang: Lang::Shell,
        code: "ce",
        code_attached: false,
        long_code: &[
            "-Command",
            "-command",
            "-EncodedCommand",
            "-encodedcommand",
            "-ec",
        ],
        valued: "",
        long_valued: NO_LONG,
        attached: "",
    },
    Interp {
        names: &["osascript"],
        lang: Lang::AppleScript,
        code: "e",
        code_attached: false,
        long_code: NO_LONG,
        valued: "ls",
        long_valued: NO_LONG,
        attached: "",
    },
];

/// An interpreter's command line, read.
pub(crate) struct Inline {
    pub lang: Lang,
    /// Every piece of code it is given (`-e` may come more than once).
    pub codes: Vec<String>,
    /// Command lines it hands to a shell (`bun exec`).
    pub lines: Vec<String>,
    /// It reads its program from standard input: no code, no module, no script (or `-`).
    pub stdin: bool,
}

/// `p`'s command line when `p` is an interpreter: all its code, attached (`-c'…'`), clustered
/// (`-ne '…'`) or repeated (`-e a -e b`). Every word is looked at, also after a script's name: a
/// word taken for code there is only read, never let through on its own.
pub(crate) fn interpreter(p: &str, args: &[String]) -> Option<Inline> {
    let it = INTERPRETERS.iter().find(|i| i.names.contains(&p))?;
    let mut out = Inline {
        lang: it.lang,
        codes: vec![],
        lines: vec![],
        stdin: false,
    };
    let mut first: Option<&str> = None;
    let mut module = false;
    let mut i = 0;
    let mut opts = true;
    while let Some(x) = args.get(i) {
        i += 1;
        if opts && x == "--" {
            opts = false;
            continue;
        }
        let long = it
            .long_code
            .iter()
            .find(|l| x == *l || x.strip_prefix(*l).is_some_and(|r| r.starts_with('=')));
        if let Some(l) = long {
            match x.strip_prefix(*l).and_then(|r| r.strip_prefix('=')) {
                Some(v) => out.codes.push(v.into()),
                None => {
                    out.codes.extend(args.get(i).cloned());
                    i += 1;
                }
            }
            continue;
        }
        if x.starts_with("--") {
            if it.long_valued.contains(&x.as_str()) {
                i += 1;
            }
            continue;
        }
        if opts && x.len() > 1 && x.starts_with('-') {
            let cs: Vec<char> = x[1..].chars().collect();
            for (j, &c) in cs.iter().enumerate() {
                let rest: String = cs[j + 1..].iter().collect();
                if it.code.contains(c) {
                    if it.code_attached && !rest.is_empty() {
                        out.codes.push(rest);
                    } else {
                        out.codes.extend(args.get(i).cloned());
                        i += 1;
                    }
                    break;
                }
                if it.valued.contains(c) {
                    module |= it.lang == Lang::Python && c == 'm';
                    if rest.is_empty() {
                        i += 1;
                    }
                    break;
                }
                if it.attached.contains(c) {
                    break;
                }
            }
            continue;
        }
        first.get_or_insert(x.as_str());
    }
    match (p, first) {
        ("deno", Some("eval")) => out
            .codes
            .extend(operands(args).get(1).map(|s| s.to_string())),
        ("deno", Some("run")) => out.stdin = operands(args).get(1).is_none_or(|s| *s == "-"),
        ("bun", Some("exec")) => out
            .lines
            .extend(operands(args).get(1).map(|s| s.to_string())),
        _ => {}
    }
    if p != "deno" && out.codes.is_empty() && !module {
        out.stdin = first.is_none_or(|f| f == "-");
    } else if p == "deno" && first.is_none() {
        out.stdin = true;
    }
    Some(out)
}

/// Words in code, in any of the languages, that start programs, call into native code, signal a
/// process or evaluate text as code.
static RUNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(system\w*|popen\w*|spawn\w*|exec\w*|eval|fork\w*|subprocess|child_process",
        r"|posix_spawn\w*|startfile|webbrowser|pty|ctypes|cffi|ffi|fiddle|dlopen|syscall|setuid",
        r"|seteuid|setreuid|setresuid|setgid|setegid|setgroups|initgroups|open3|shell_exec",
        r"|passthru|proc_open|pcntl_\w+|posix_\w+)\b|\w*kill\w*|create_subprocess",
    ))
    .expect("valid regex")
});
/// Per language: loading code by a computed name, reaching a function by one (reflection),
/// unpickling.
static PY_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(__import__|importlib|runpy|getattr|setattr|delattr|globals|locals|vars|inspect",
        r"|attrgetter|methodcaller|pickle|cPickle|marshal|dill|shelve|jsonpickle|unsafe_load",
        r"|builtins|code\.interact)\b|\byaml\.load\b",
    ))
    .expect("valid regex")
});
static JS_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(Function|constructor|__proto__|Reflect|globalThis|global|vm|worker_threads|Worker",
        r"|WebAssembly|mainModule|binding|_linkedBinding|getBuiltinModule|Deno|Bun)\b",
    ))
    .expect("valid regex")
});
static RUBY_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(send|public_send|__send__|instance_eval|class_eval|module_eval|instance_exec",
        r"|const_get|define_method|method|Kernel|ObjectSpace|binding|DL)\b",
    ))
    .expect("valid regex")
});
static PERL_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(can|UNIVERSAL|CORE|do\s*['\x22$]|require\s*\$)").expect("valid regex")
});
static PHP_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(call_user_func\w*|create_function|assert|include|require\w*)\b|\$\w+\s*\(")
        .expect("valid regex")
});
static LUA_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(load|loadstring|loadfile|dofile|require|debug|package)\b").expect("valid regex")
});
static R_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(shell|pipe|do\.call|match\.fun|get|mget|parse|source|processx|callr|dyn\.load",
        r"|sys\.function|body|environment)\b|\.(Call|External|C)\b",
    ))
    .expect("valid regex")
});
/// Text written with escapes (`\x63`, `\u0063`, `\143`) may spell any name.
static ESCAPES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(x[0-9A-Fa-f]{2}|u\{?[0-9A-Fa-f]|U[0-9A-Fa-f]{8}|[0-7]{3}|N\{)")
        .expect("valid regex")
});
/// Python's double-underscore names reach everything by reflection; these few only name things.
static DUNDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"__(\w+?)__").expect("valid regex"));
const PLAIN_DUNDERS: &[&str] = &["name", "main", "file", "doc", "version", "init", "all"];
/// JavaScript: what of `process` only reads or writes this process's own state.
static PROCESS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bprocess\b(\s*\.\s*(\w+))?").expect("valid regex"));
const PROCESS_OWN: &[&str] = &[
    "argv",
    "env",
    "stdout",
    "stderr",
    "stdin",
    "exit",
    "exitCode",
    "cwd",
    "platform",
    "version",
    "versions",
    "arch",
    "pid",
    "ppid",
    "hrtime",
    "memoryUsage",
    "uptime",
    "nextTick",
    "on",
    "once",
    "emitWarning",
];
/// JavaScript: `require(…)`/`import(…)` of a name written out as one literal.
static LOADS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(require|import)\s*\(").expect("valid regex"));
static LITERAL_ARG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*('[^'\\]*'|"[^"\\]*")\s*\)"#).expect("valid regex"));
/// JavaScript: a member reached by a computed key (`o[k]`, `o["a"+"b"]`, `o[f()]`).
static COMPUTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"[\w\])]\s*\[\s*([A-Za-z_$`]|[^\]]*[+(`])"#).expect("valid regex")
});
/// Ruby, Perl and PHP: backquotes, `%x`, `qx`, a pipe opened as a file, a symbolic or code-ref
/// call.
static SHELLOUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"`|%x|\bqx\b|\bopen\b[^;]*['\x22]\s*-?\|",
        r"|&\s*[\{\$]|\*\s*\{|->\s*[\$\(]",
    ))
    .expect("valid regex")
});
/// awk: `system()`, reading a command (`cmd | getline`) or printing to one.
static AWK_RUNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bsystem\s*\(|\|\s*&?\s*getline|\bprintf?\b[^;{}]*\|").expect("valid regex")
});

/// Why `code` cannot be read for what it runs, or `None`: it names none of the ways its language
/// starts a program, evaluates text, reaches a name it computes, or signals a process. This is a
/// reader, not a sandbox: it only lets through code that plainly computes, reads and writes files;
/// anything that could run something else stays with the user, whatever its strings say.
pub(crate) fn code_runs(lang: Lang, code: &str) -> Option<String> {
    let why = |what: &str| {
        Some(format!(
            "内联代码里有能运行别的程序或动态执行的写法（{what}），静态看不出会运行什么，留给你"
        ))
    };
    match lang {
        Lang::Shell => return why("PowerShell 命令"),
        Lang::AppleScript => return why("AppleScript"),
        Lang::Awk => {
            return AWK_RUNS.find(code).and_then(|m| why(m.as_str()));
        }
        _ => {}
    }
    if let Some(m) = RUNS.find(code) {
        return why(m.as_str());
    }
    if let Some(m) = ESCAPES.find(code) {
        return why(m.as_str());
    }
    let dynamic = match lang {
        Lang::Python => &PY_DYNAMIC,
        Lang::Js => &JS_DYNAMIC,
        Lang::Ruby => &RUBY_DYNAMIC,
        Lang::Perl => &PERL_DYNAMIC,
        Lang::Php => &PHP_DYNAMIC,
        Lang::Lua => &LUA_DYNAMIC,
        Lang::R => &R_DYNAMIC,
        Lang::Awk | Lang::Shell | Lang::AppleScript => return None,
    };
    if let Some(m) = dynamic.find(code) {
        return why(m.as_str());
    }
    match lang {
        // double-underscore names reach everything by reflection; a few only name things
        Lang::Python => {
            if let Some(c) = DUNDER
                .captures_iter(code)
                .find(|c| !PLAIN_DUNDERS.contains(&&c[1]))
            {
                return why(&c[0]);
            }
        }
        Lang::Js => {
            if let Some(c) = PROCESS
                .captures_iter(code)
                .find(|c| c.get(2).is_none_or(|m| !PROCESS_OWN.contains(&m.as_str())))
            {
                return why(&c[0]);
            }
            if let Some(m) = LOADS
                .find_iter(code)
                .find(|m| !LITERAL_ARG.is_match(&code[m.end()..]))
            {
                return why(m.as_str());
            }
            if let Some(m) = COMPUTED.find(code) {
                return why(m.as_str());
            }
        }
        Lang::Ruby | Lang::Perl | Lang::Php => {
            if let Some(m) = SHELLOUT.find(code) {
                return why(m.as_str());
            }
        }
        _ => {}
    }
    None
}

// ── sed ──

/// Why a sed command line runs something: a script that executes (GNU's `e` command and `s///e`
/// flag, whatever the platform's sed supports) or one that cannot be read (in a file, or not a
/// script this reader knows). `None`: its scripts only edit text.
pub(crate) fn sed_runs(args: &[String]) -> Option<String> {
    const IN_FILE: &str = "sed 的脚本在文件里，看不出会不会执行命令";
    let mut scripts: Vec<String> = vec![];
    let mut first: Option<&String> = None;
    let mut i = 0;
    let mut opts = true;
    while let Some(x) = args.get(i) {
        i += 1;
        if !opts || !x.starts_with('-') || x == "-" {
            first.get_or_insert(x);
            continue;
        }
        if x == "--" {
            opts = false;
            continue;
        }
        if let Some(long) = x.strip_prefix("--") {
            let (n, v) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            match (n, v) {
                ("expression", Some(v)) => scripts.push(v.into()),
                ("expression", None) => {
                    scripts.extend(args.get(i).cloned());
                    i += 1;
                }
                ("file", _) => return Some(IN_FILE.into()),
                ("line-length", None) => i += 1,
                _ => {}
            }
            continue;
        }
        // a cluster of short options: `-ne SCRIPT`, `-e'SCRIPT'`, `-i.bak`
        for (j, c) in x.char_indices().skip(1) {
            let rest = &x[j + c.len_utf8()..];
            match c {
                'e' => {
                    if rest.is_empty() {
                        scripts.extend(args.get(i).cloned());
                        i += 1;
                    } else {
                        scripts.push(rest.into());
                    }
                    break;
                }
                'f' => return Some(IN_FILE.into()),
                // a suffix (`-i.bak`): BSD's `-i SUFFIX` has GNU read the suffix as the script,
                // which BSD's sed, without `e`, could not run
                'i' | 'I' => break,
                _ => {}
            }
        }
    }
    // the script is the first operand when no -e gave one
    if scripts.is_empty() {
        scripts.extend(first.cloned());
    }
    for s in &scripts {
        match sed_script(s) {
            Ok(None) => {}
            Ok(Some(what)) => return Some(format!("sed 脚本会执行命令（{what}），留给你")),
            Err(()) => return Some(format!("看不懂 sed 脚本 {s}，看不出会不会执行命令")),
        }
    }
    None
}

/// What in sed script `s` executes (`e …`, an `s` command's `e` flag); `Err`: not a script this
/// reader knows.
fn sed_script(s: &str) -> Result<Option<String>, ()> {
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let at = |i: usize| cs.get(i).copied();
    let skip_blank = |i: &mut usize| {
        while matches!(cs.get(*i), Some(' ' | '\t')) {
            *i += 1;
        }
    };
    // the rest of the line (a file name, text, a command); a backslash-newline continues it
    let to_eol = |i: &mut usize| -> String {
        let mut out = String::new();
        while let Some(&c) = cs.get(*i) {
            if c == '\\' && cs.get(*i + 1) == Some(&'\n') {
                out.push('\n');
                *i += 2;
                continue;
            }
            if c == '\n' {
                break;
            }
            out.push(c);
            *i += 1;
        }
        out
    };
    // a delimited part (`/re/`, `\%re%`, the parts of `s` and `y`), escapes kept
    let part = |i: &mut usize, d: char| -> Result<(), ()> {
        while let Some(&c) = cs.get(*i) {
            *i += 1;
            if c == '\\' {
                *i += 1;
            } else if c == d {
                return Ok(());
            }
        }
        Err(())
    };
    let address = |i: &mut usize| -> Result<(), ()> {
        match at(*i) {
            Some(c) if c.is_ascii_digit() => {
                while at(*i).is_some_and(|c| c.is_ascii_digit() || c == '~') {
                    *i += 1;
                }
            }
            Some('$') => *i += 1,
            Some('/') => {
                *i += 1;
                part(i, '/')?;
                while matches!(at(*i), Some('I' | 'M')) {
                    *i += 1;
                }
            }
            Some('\\') => {
                let d = at(*i + 1).ok_or(())?;
                *i += 2;
                part(i, d)?;
                while matches!(at(*i), Some('I' | 'M')) {
                    *i += 1;
                }
            }
            _ => {}
        }
        Ok(())
    };
    loop {
        while matches!(at(i), Some(' ' | '\t' | '\n' | ';' | '}')) {
            i += 1;
        }
        let Some(c) = at(i) else {
            return Ok(None);
        };
        if c == '#' {
            to_eol(&mut i);
            continue;
        }
        address(&mut i)?;
        skip_blank(&mut i);
        if at(i) == Some(',') {
            i += 1;
            skip_blank(&mut i);
            if matches!(at(i), Some('+' | '~')) {
                i += 1;
            }
            address(&mut i)?;
        }
        skip_blank(&mut i);
        while at(i) == Some('!') {
            i += 1;
            skip_blank(&mut i);
        }
        let Some(cmd) = at(i) else {
            return Err(());
        };
        i += 1;
        match cmd {
            '{' => continue,
            '=' | 'd' | 'D' | 'g' | 'G' | 'h' | 'H' | 'n' | 'N' | 'p' | 'P' | 'x' | 'z' | 'F' => {}
            'l' | 'L' | 'q' | 'Q' => {
                skip_blank(&mut i);
                while at(i).is_some_and(|c| c.is_ascii_digit()) {
                    i += 1;
                }
            }
            'b' | 't' | 'T' | ':' | 'v' => {
                while at(i).is_some_and(|c| c != ';' && c != '\n') {
                    i += 1;
                }
            }
            'a' | 'i' | 'c' | 'r' | 'R' | 'w' | 'W' => {
                to_eol(&mut i);
            }
            'e' => {
                skip_blank(&mut i);
                let run = to_eol(&mut i);
                return Ok(Some(if run.trim().is_empty() {
                    "e".into()
                } else {
                    format!("e {}", run.trim())
                }));
            }
            's' | 'y' => {
                let d = at(i).ok_or(())?;
                if d == '\\' || d == '\n' {
                    return Err(());
                }
                i += 1;
                part(&mut i, d)?;
                part(&mut i, d)?;
                if cmd == 's' {
                    while let Some(f) = at(i) {
                        match f {
                            'e' => return Ok(Some("s///e".into())),
                            'g' | 'p' | 'i' | 'I' | 'm' | 'M' => i += 1,
                            d if d.is_ascii_digit() => i += 1,
                            'w' => {
                                to_eol(&mut i);
                                break;
                            }
                            _ => break,
                        }
                    }
                }
            }
            _ => return Err(()),
        }
        skip_blank(&mut i);
        if !matches!(at(i), None | Some(';' | '\n' | '}' | '#')) {
            return Err(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn sed_scripts_that_execute_are_found() {
        for s in [
            "1e pkill -f node",
            "e",
            "s/.*/x/e",
            "s/a/b/ge",
            "/x/{e id\n}",
            "$!N;s/x/y/;1e id",
            "1~2e id",
            "0,/re/e id",
            "/a/,+2 e id",
            "\\%x%e id",
            "s|a|b|w out.txt\n1e id",
        ] {
            let s = s.replace("\\n", "\n");
            assert!(matches!(sed_script(&s), Ok(Some(_))), "{s:?}");
        }
        for s in [
            "",
            "1p",
            "s/a/e/g",
            "s/e/E/2",
            "/exit/d",
            "/e/!d",
            "y/abe/xyz/",
            "1i\\\nheader",
            "1a text with e in it; and more",
            "$a end",
            ":top\nN;btop",
            "s/x/y/w out.e",
            "# comment e\np",
            "/^$/d",
            "s/(e)/\\1x/",
            "l 40",
            "q5",
        ] {
            let s = s.replace("\\n", "\n");
            assert_eq!(sed_script(&s), Ok(None), "{s:?}");
        }
        assert!(sed_script("k").is_err(), "not a sed command");
        assert!(sed_script("s/a/b").is_err(), "unclosed");
    }

    #[test]
    fn sed_command_lines_are_read_like_sed_reads_them() {
        assert!(sed_runs(&w(&["-n", "1e id", "f"])).is_some());
        assert!(sed_runs(&w(&["-ne", "1e id", "f"])).is_some());
        assert!(sed_runs(&w(&["-e1e id", "f"])).is_some());
        assert!(sed_runs(&w(&["--expression", "1e id", "f"])).is_some());
        assert!(sed_runs(&w(&["-e", "p", "-e", "s/x/y/e", "f"])).is_some());
        assert!(sed_runs(&w(&["-f", "x.sed", "f"])).is_some());
        assert!(sed_runs(&w(&["--file=x.sed", "f"])).is_some());
        // the files are not scripts: `e.txt` is a file
        assert!(sed_runs(&w(&["-i", "s/a/b/", "e.txt"])).is_none());
        assert!(sed_runs(&w(&["-i", "", "s/a/b/", "e.txt"])).is_none());
        assert!(sed_runs(&w(&["-e", "s/a/b/", "e.txt"])).is_none());
        assert!(sed_runs(&w(&["-i.bak", "s/a/b/", "e.txt"])).is_none());
    }

    #[test]
    fn every_piece_of_inline_code_is_found() {
        let codes = |p: &str, a: &[&str]| interpreter(p, &w(a)).map(|i| (i.codes, i.stdin));
        assert_eq!(codes("python3", &["-cA"]), Some((w(&["A"]), false)));
        assert_eq!(codes("python3", &["-Ic", "A"]), Some((w(&["A"]), false)));
        assert_eq!(
            codes("python3", &["-W", "x", "-c", "A"]),
            Some((w(&["A"]), false))
        );
        assert_eq!(
            codes("python3", &["-m", "http.server"]),
            Some((w(&[]), false))
        );
        assert_eq!(codes("python3", &["-"]), Some((w(&[]), true)));
        assert_eq!(codes("python3", &[]), Some((w(&[]), true)));
        assert_eq!(codes("python3", &["tool.py"]), Some((w(&[]), false)));
        assert_eq!(
            codes("ruby", &["-e", "A", "-e", "B"]),
            Some((w(&["A", "B"]), false))
        );
        assert_eq!(codes("ruby", &["-eA"]), Some((w(&["A"]), false)));
        assert_eq!(codes("perl", &["-ne", "A", "f"]), Some((w(&["A"]), false)));
        assert_eq!(
            codes("perl", &["-pi", "-e", "A", "f"]),
            Some((w(&["A"]), false))
        );
        assert_eq!(
            codes("perl", &["-i.bak", "-pe", "A"]),
            Some((w(&["A"]), false))
        );
        assert_eq!(codes("node", &["-pe", "A"]), Some((w(&["A"]), false)));
        assert_eq!(codes("node", &["--eval=A"]), Some((w(&["A"]), false)));
        assert_eq!(
            codes("node", &["-r", "x", "-e", "A"]),
            Some((w(&["A"]), false))
        );
        assert_eq!(codes("node", &["app.js"]), Some((w(&[]), false)));
        assert_eq!(codes("deno", &["eval", "A"]), Some((w(&["A"]), false)));
        assert_eq!(
            codes("Rscript", &["-e", "A", "-e", "B"]),
            Some((w(&["A", "B"]), false))
        );
        assert_eq!(codes("ls", &["-e"]), None);
    }

    #[test]
    fn code_that_could_run_something_else_is_named() {
        for (l, c) in [
            (Lang::Python, "import os; os.system('x')"),
            (Lang::Python, "getattr(m, 'n')()"),
            (Lang::Python, "__builtins__"),
            (Lang::Python, "importlib.import_module(n)"),
            (Lang::Js, "process.kill(1)"),
            (Lang::Js, "process['ki'+'ll'](1)"),
            (Lang::Js, "require(name)"),
            (Lang::Js, "import('child_' + 'process')"),
            (Lang::Js, "o[k]()"),
            (Lang::Js, "[].constructor.constructor('x')()"),
            (Lang::Js, "globalThis.x"),
            (Lang::Ruby, "`id`"),
            (Lang::Ruby, "o.public_send(:x)"),
            (Lang::Perl, "qx(id)"),
            (Lang::Perl, "open(F, '|id')"),
            (Lang::Php, "$f('x')"),
            (Lang::Lua, "load('x')()"),
            (Lang::R, "do.call('x', list())"),
            (Lang::Awk, "{ system(\"id\") }"),
            (Lang::Awk, "{ \"id\" | getline x }"),
        ] {
            assert!(code_runs(l, c).is_some(), "{l:?} {c}");
        }
        for (l, c) in [
            (Lang::Python, "print(sum(range(3)))"),
            (
                Lang::Python,
                "import json,sys; print(json.load(sys.stdin)['v'])",
            ),
            (Lang::Python, "if __name__ == '__main__': print(__file__)"),
            (Lang::Js, "console.log(require('./package.json').version)"),
            (Lang::Js, "console.log(process.argv[2], process.env.HOME)"),
            (Lang::Js, "console.log(JSON.parse(s)['k'], a[0])"),
            (Lang::Ruby, "puts [1,2].sum"),
            (Lang::Perl, "print if /x/"),
            (Lang::Awk, "{ print $1 > \"out.txt\" }"),
            (Lang::Awk, "{ if (a || b) print }"),
        ] {
            assert_eq!(code_runs(l, c), None, "{l:?} {c}");
        }
    }
}
