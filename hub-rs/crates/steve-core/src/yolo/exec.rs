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

/// Names of a program's standard input as a file: a script read from there is fed, not written.
const STDIN_PATHS: &[&str] = &["-", "/dev/stdin", "/dev/fd/0", "/proc/self/fd/0"];

/// A shell reads its script from standard input: `-s`, or no script operand (or one that names
/// standard input). `-o`/`-O` take a value.
fn shell_reads_stdin(args: &[String]) -> bool {
    let mut i = 0;
    while let Some(x) = args.get(i) {
        i += 1;
        match x.as_str() {
            "--" => break,
            "-o" | "+o" | "-O" | "+O" => i += 1,
            s if s.starts_with("--") => {}
            s if s.starts_with('-') && s.len() > 1 => {
                if s[1..].contains('s') {
                    return true;
                }
            }
            s if s.starts_with('+') => {}
            s => return STDIN_PATHS.contains(&s),
        }
    }
    args.get(i)
        .is_none_or(|s| STDIN_PATHS.contains(&s.as_str()))
}

/// Variables whose value a program runs as a command line (an editor, a pager, an ssh command,
/// an askpass helper): `GIT_EDITOR='…' git commit`, `export PAGER=…`.
const COMMAND_VARS: &[&str] = &[
    "EDITOR",
    "VISUAL",
    "PAGER",
    "MANPAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SUDO_ASKPASS",
    "GIT_EXTERNAL_DIFF",
    "PROMPT_COMMAND",
    "LESSOPEN",
    "LESSCLOSE",
    "BROWSER",
    // build tools run these programs (a compiler, its wrapper, a test runner)
    "RUSTC",
    "RUSTDOC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CC",
    "CXX",
    "LD",
    "MAKE",
    "SHELL",
];
/// Variables that load code into the programs that follow (a preloaded library, interpreter or
/// git options, a shell's startup file): what they run cannot be read off the line.
const CODE_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_AUDIT",
    "NODE_OPTIONS",
    "PERL5OPT",
    "RUBYOPT",
    "PYTHONBREAKPOINT",
    "PYTHONSTARTUP",
    "BASH_ENV",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
];

/// The variable assignments of a simple command: the ones in front of its program (and of the
/// wrappers before it: `env A=… cmd`, `sudo A=… cmd`), the arguments of `export`/`declare`…, and
/// make's variable arguments (`make CC=…`). A `NAME=value` word given to any other program is
/// its data (`echo ENV=prod`, `docker run -e A=b`).
fn assignments(words: &[String]) -> Vec<&String> {
    const SETS: &[&str] = &["export", "declare", "typeset", "local", "readonly"];
    const PASS: &[&str] = &[
        "env",
        "sudo",
        "doas",
        "nohup",
        "nice",
        "time",
        "command",
        "exec",
        "stdbuf",
        "caffeinate",
    ];
    let mut out = vec![];
    for (i, w) in words.iter().enumerate() {
        if shell::is_assignment(w) {
            out.push(w);
            continue;
        }
        let p = shell::basename(w);
        if SETS.contains(&p) || matches!(p, "make" | "gmake") {
            out.extend(words[i + 1..].iter().filter(|x| shell::is_assignment(x)));
            break;
        }
        // a wrapper and its options: assignments may follow (`env -i A=b cmd`)
        if PASS.contains(&p) || w.starts_with('-') {
            continue;
        }
        break;
    }
    out
}

/// What the variable assignments of a simple command ([`assignments`]) make the programs after
/// them run: a command variable's value is a line; a variable that loads code cannot be read.
pub(crate) fn var_bodies(words: &[String]) -> Vec<Body> {
    let mut out = vec![];
    for w in assignments(words) {
        let Some((k, v)) = w.split_once('=') else {
            continue;
        };
        if v.trim().is_empty() {
            continue;
        }
        let loads = match k {
            // only these options load code; `--max-old-space-size` does not
            "NODE_OPTIONS" => [
                "--require",
                "-r",
                "--import",
                "--loader",
                "--experimental-loader",
                "--eval",
                "-e",
            ]
            .iter()
            .any(|o| {
                v.split_whitespace()
                    .any(|x| x == *o || x.starts_with(&format!("{o}=")))
            }),
            "PYTHONBREAKPOINT" => v != "0",
            _ => {
                CODE_VARS.contains(&k)
                    || k.starts_with("DYLD_")
                    || k.starts_with("GIT_CONFIG_KEY_")
                    || k.starts_with("GIT_CONFIG_VALUE_")
            }
        };
        if COMMAND_VARS.contains(&k)
            || k.starts_with("CARGO_TARGET_") && (k.ends_with("_RUNNER") || k.ends_with("_LINKER"))
        {
            out.push(Body::Line(v.to_string(), Feed::default()));
        } else if loads {
            out.push(Body::Unseen(format!(
                "环境变量 {k} 会让后面的程序加载代码，看不出会运行什么"
            )));
        }
    }
    out
}

/// What an option of an ordinary program runs: its value is a command line (`split --filter`),
/// a program (`rg --pre`), the rest of the words up to `;` (`fd -x`), or something that cannot
/// be read (a vim command, a crontab).
enum Runs {
    Line,
    Program,
    Rest,
    Unseen,
}

/// Programs (by their names) and the options of theirs that run a command.
type OptionCommands = (&'static [&'static str], &'static [(&'static str, Runs)]);

/// Programs that run a command given in one of their options, and those options. Their other words
/// are data.
const OPTION_COMMANDS: &[OptionCommands] = &[
    (
        &["fd", "fdfind"],
        &[
            ("-x", Runs::Rest),
            ("--exec", Runs::Rest),
            ("-X", Runs::Rest),
            ("--exec-batch", Runs::Rest),
        ],
    ),
    (&["rg"], &[("--pre", Runs::Program)]),
    (
        &["tar", "gtar", "bsdtar"],
        &[
            ("--to-command", Runs::Line),
            ("-I", Runs::Line),
            ("--use-compress-program", Runs::Line),
            ("--checkpoint-action", Runs::Unseen),
            ("-F", Runs::Line),
            ("--info-script", Runs::Line),
            ("--new-volume-script", Runs::Line),
        ],
    ),
    (&["sort", "gsort"], &[("--compress-program", Runs::Program)]),
    (&["split", "gsplit"], &[("--filter", Runs::Line)]),
    (&["man"], &[("-P", Runs::Line), ("--pager", Runs::Line)]),
    (
        &["zip"],
        &[("-TT", Runs::Line), ("--unzip-command", Runs::Line)],
    ),
    (
        &["vim", "vi", "nvim", "view", "ex"],
        &[
            ("-c", Runs::Unseen),
            ("--cmd", Runs::Unseen),
            ("+", Runs::Unseen),
        ],
    ),
    (&["less"], &[("+", Runs::Unseen)]),
];

/// What `p`'s command options run (`fd -x`, `tar --to-command`…).
pub(crate) fn option_bodies(p: &str, args: &[String], feed: &Feed) -> Vec<Body> {
    let mut out = vec![];
    let Some((_, opts)) = OPTION_COMMANDS.iter().find(|(n, _)| n.contains(&p)) else {
        return out;
    };
    let mut i = 0;
    while let Some(x) = args.get(i) {
        i += 1;
        for (o, runs) in opts.iter() {
            // `--opt=v`, `--opt v`, `-Ov`, `-O v`, `+cmd`
            let value = if *o == "+" {
                x.strip_prefix('+').map(String::from)
            } else if x == o {
                let v = args.get(i).cloned();
                if !matches!(runs, Runs::Rest) {
                    i += 1;
                }
                v
            } else if o.starts_with("--") {
                x.strip_prefix(&format!("{o}=")).map(String::from)
            } else {
                x.strip_prefix(o)
                    .filter(|v| !v.is_empty())
                    .map(String::from)
            };
            let Some(v) = value else {
                continue;
            };
            match runs {
                Runs::Line => out.push(Body::Line(v, feed.clone())),
                Runs::Program => out.push(Body::Argv(vec![v], feed.clone())),
                Runs::Rest => {
                    // `-xcmd` / `--exec=cmd`: the attached word is the program
                    let attached = (x != o).then(|| v.clone());
                    let end = args[i..]
                        .iter()
                        .position(|x| x == ";" || x == "\\;")
                        .map_or(args.len(), |e| i + e);
                    let mut words: Vec<String> = attached.into_iter().collect();
                    words.extend(args[i..end].iter().cloned());
                    out.push(Body::Argv(words, feed.clone()));
                    i = end;
                }
                Runs::Unseen => out.push(Body::Unseen(format!(
                    "{p} 的 {o} 会运行这里读不出来的命令，留给你"
                ))),
            }
            break;
        }
    }
    out
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
    "firejail",
    "proxychains",
    "proxychains4",
    "torsocks",
    "chpst",
    "setpriv",
    "capsh",
    "sandbox-exec",
    "perf",
    "flatpak-spawn",
    "systemd-inhibit",
    "sg",
    "newgrp",
    "numactl",
    "cgexec",
    // a job that runs later
    "at",
    "batch",
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
        names: &["xvfb-run"],
        flags: "a",
        valued: "efnpsw",
        attached: "",
        long_flags: &["--auto-servernum", "--help"],
        long_valued: &[
            "--error-file",
            "--auth-file",
            "--server-num",
            "--xauth-protocol",
            "--server-args",
            "--wait",
        ],
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["dbus-run-session"],
        flags: "",
        valued: "",
        attached: "",
        long_flags: &["--config-file", "--dbus-daemon", "--help", "--version"],
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["ssh-agent"],
        flags: "csDdk",
        valued: "aEPt",
        attached: "",
        long_flags: NO_LONG,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["catchsegv"],
        flags: "",
        valued: "",
        attached: "",
        long_flags: HELP,
        long_valued: NO_LONG,
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
            // `bash script.sh` runs a file, as any program would; `-s`, no script or
            // `/dev/stdin` read the script from standard input
            None if shell_reads_stdin(args) => from_stdin(&mut out, None),
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
                .is_none_or(|f| STDIN_PATHS.contains(&f.as_str()))
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
    if let Some(b) = sub_runner(p, args, feed) {
        out.extend(b);
        return out;
    }
    if let Some(it) = interpreter(p, args) {
        if let Some(why) = it.unseen {
            out.push(Body::Unseen(why));
            return out;
        }
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
    if p == "cargo"
        && args.iter().enumerate().any(|(i, x)| {
            let v = match x.strip_prefix("--config") {
                Some("") => args.get(i + 1).map(String::as_str).unwrap_or(""),
                Some(r) => r.strip_prefix('=').unwrap_or(""),
                None => "",
            };
            ["runner", "wrapper", "linker", "rustc", "rustdoc"]
                .iter()
                .any(|k| v.contains(k))
        })
    {
        out.push(Body::Unseen(
            "cargo --config 指定了要运行的程序，看不出会运行什么".into(),
        ));
        return out;
    }
    if p == "crontab" && !args.iter().all(|x| x == "-l") {
        // a job installed now runs later, whatever it is
        out.push(Body::Unseen("crontab 装的任务以后才运行，留给你".into()));
        return out;
    }
    let ob = option_bodies(p, args, feed);
    if !ob.is_empty() {
        out.extend(ob);
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

/// A package or environment manager that runs a command of its own (`npm exec`, `bundle exec`,
/// `uv run`, `mise exec … --`…), read by the option words it takes: `valued` take the next word
/// (unless written `--x=v`), `line` take a command line, `shell` make the command words a line,
/// `none` make it run no program read here (a Python module, a lookup), `flags` take nothing. An
/// option not listed leaves where its command starts unknown. Its command follows the options
/// (and `lead` more words), or follows `--`; with `dashdash`, only `--` or a `line` option give one.
struct SubRunner {
    names: &'static [&'static str],
    /// The subcommand that runs a command (`exec`, `run`); empty: the program itself does.
    subs: &'static [&'static str],
    flags: &'static [&'static str],
    valued: &'static [&'static str],
    line: &'static [&'static str],
    shell: &'static [&'static str],
    none: &'static [&'static str],
    lead: usize,
    dashdash: bool,
}

const SUB_RUNNERS: &[SubRunner] = &[
    SubRunner {
        names: &["npm"],
        subs: &["exec", "x"],
        flags: &[
            "--yes",
            "-y",
            "--no",
            "--workspaces",
            "--ws",
            "--include-workspace-root",
            "--silent",
            "-s",
            "--quiet",
            "-q",
            "--offline",
            "--prefer-offline",
            "--prefer-online",
            "--legacy-peer-deps",
        ],
        valued: &[
            "--package",
            "-p",
            "--workspace",
            "-w",
            "--prefix",
            "--registry",
            "--cache",
            "--userconfig",
            "--loglevel",
        ],
        line: &["--call", "-c"],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["pnpm"],
        subs: &["exec", "dlx"],
        flags: &[
            "--recursive",
            "-r",
            "--parallel",
            "--no-bail",
            "--silent",
            "-s",
            "--workspace-root",
            "-w",
            "--report-summary",
            "--reverse",
        ],
        valued: &[
            "--package",
            "--filter",
            "-F",
            "--dir",
            "-C",
            "--workspace-dir",
            "--resume-from",
        ],
        line: &[],
        shell: &["-c", "--shell-mode"],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["yarn"],
        subs: &["exec", "dlx"],
        flags: &["--quiet", "-q"],
        valued: &["--package", "-p"],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["bun"],
        subs: &["x"],
        flags: &["--bun"],
        valued: &[],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["bundle", "bundler"],
        subs: &["exec"],
        flags: &["--keep-file-descriptors"],
        valued: &["--gemfile"],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["direnv"],
        subs: &["exec"],
        flags: &[],
        valued: &[],
        line: &[],
        shell: &[],
        none: &[],
        lead: 1,
        dashdash: false,
    },
    SubRunner {
        names: &["uv"],
        subs: &["run"],
        flags: &[
            "--frozen",
            "--locked",
            "--no-sync",
            "--isolated",
            "--no-project",
            "--no-dev",
            "--dev",
            "--only-dev",
            "--all-extras",
            "--all-packages",
            "--active",
            "--no-env-file",
            "--no-editable",
            "--exact",
            "--offline",
            "--no-cache",
            "--quiet",
            "-q",
            "--verbose",
            "-v",
        ],
        valued: &[
            "--with",
            "--with-requirements",
            "--with-editable",
            "--python",
            "-p",
            "--package",
            "--extra",
            "--group",
            "--only-group",
            "--no-group",
            "--index",
            "--index-url",
            "--default-index",
            "--extra-index-url",
            "--env-file",
            "--directory",
            "--project",
            "--config-setting",
            "-C",
            "--cache-dir",
        ],
        line: &[],
        shell: &[],
        none: &["-m", "--module", "--script", "--gui-script"],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["poetry"],
        subs: &["run"],
        flags: &[
            "--quiet",
            "-q",
            "--verbose",
            "-v",
            "-vv",
            "-vvv",
            "--no-interaction",
            "-n",
            "--no-ansi",
            "--ansi",
            "--no-plugins",
            "--no-cache",
        ],
        valued: &["--directory", "-C", "--project", "-P"],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["pipenv"],
        subs: &["run"],
        flags: &["--quiet", "-q", "--verbose", "-v"],
        valued: &["--python"],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["pdm"],
        subs: &["run"],
        flags: &[
            "--verbose",
            "-v",
            "--quiet",
            "-q",
            "--global",
            "-g",
            "--recreate",
        ],
        valued: &["--project", "-p", "--venv"],
        line: &[],
        shell: &[],
        none: &["--list", "-l"],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["conda", "mamba", "micromamba"],
        subs: &["run"],
        flags: &[
            "--live-stream",
            "--no-capture-output",
            "--debug-wrapper-scripts",
            "-v",
            "--verbose",
        ],
        valued: &["-n", "--name", "-p", "--prefix", "--cwd"],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["mise", "rtx"],
        subs: &["exec", "x"],
        flags: &[],
        valued: &[],
        line: &["-c", "--command"],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: true,
    },
    SubRunner {
        names: &["asdf", "rbenv", "pyenv", "nodenv", "goenv", "jenv"],
        subs: &["exec"],
        flags: &[],
        valued: &[],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["doppler"],
        subs: &["run"],
        flags: &[],
        valued: &[],
        line: &["--command"],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: true,
    },
    SubRunner {
        names: &["op"],
        subs: &["run"],
        flags: &[],
        valued: &[],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: true,
    },
    SubRunner {
        names: &["aws-vault"],
        subs: &["exec"],
        flags: &[
            "--no-session",
            "-n",
            "--json",
            "-j",
            "--server",
            "-s",
            "--lazy",
            "--stdout",
        ],
        valued: &[
            "--duration",
            "-d",
            "--mfa-token",
            "-t",
            "--region",
            "--prompt",
        ],
        line: &[],
        shell: &[],
        none: &[],
        lead: 1,
        dashdash: false,
    },
    SubRunner {
        names: &["dotenv"],
        subs: &[],
        flags: &["--debug", "-o", "--override", "--quiet", "-q"],
        valued: &["-e", "-v"],
        line: &[],
        shell: &[],
        none: &["-p"],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["dotenvx"],
        subs: &["run"],
        flags: &[],
        valued: &[],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: true,
    },
    SubRunner {
        names: &["nix-shell"],
        subs: &[],
        flags: &[],
        valued: &[],
        line: &["--run", "--command"],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: true,
    },
    SubRunner {
        names: &["devbox"],
        subs: &["run"],
        flags: &["--quiet", "-q"],
        valued: &["--config", "-c", "--environment", "-e"],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
    SubRunner {
        names: &["cross-env", "ccache", "sccache", "distcc"],
        subs: &[],
        flags: &[],
        valued: &[],
        line: &[],
        shell: &[],
        none: &[],
        lead: 0,
        dashdash: false,
    },
];

/// What a [`SubRunner`] runs; `None`: `p` is none, or not in the form that runs a command (`npm
/// install`, `bun run x`).
pub(crate) fn sub_runner(p: &str, args: &[String], feed: &Feed) -> Option<Vec<Body>> {
    let r = SUB_RUNNERS.iter().find(|r| r.names.contains(&p))?;
    // what `--` ends: the runner's own words
    let dash = args.iter().position(|x| x == "--");
    let own = &args[..dash.unwrap_or(args.len())];
    let name = |w: &str| {
        w.split_once('=')
            .map_or(w.to_string(), |(n, _)| n.to_string())
    };
    let mut i = 0;
    if !r.subs.is_empty() {
        // the subcommand, after global options only (`npm --prefix x exec`, not `npm install exec`)
        let mut k = 0;
        let at = loop {
            let x = own.get(k)?;
            if !x.starts_with('-') {
                if !r.subs.contains(&x.as_str()) {
                    return None;
                }
                break k;
            }
            k += if r.valued.contains(&name(x).as_str()) && !x.contains('=') {
                2
            } else {
                1
            };
        };
        i = at + 1;
    } else if matches!(p, "ccache" | "sccache" | "distcc")
        && args.first().is_some_and(|x| x.starts_with('-'))
    {
        // `ccache -s`: its own statistics, no compiler run
        return Some(vec![]);
    }
    let mut out = vec![];
    if r.dashdash {
        // a line may be given anywhere among its own words (`nix-shell -p x --run '…'`)
        let mut j = i;
        while let Some(x) = own.get(j) {
            j += 1;
            if r.line.contains(&name(x).as_str()) {
                match x.split_once('=') {
                    Some((_, v)) => out.push(Body::Line(v.to_string(), feed.clone())),
                    None => {
                        out.extend(own.get(j).map(|l| Body::Line(l.clone(), feed.clone())));
                        j += 1;
                    }
                }
            }
        }
        if let Some(d) = dash {
            let words = args[d + 1..].to_vec();
            if !words.is_empty() {
                out.push(Body::Argv(words, feed.clone()));
            }
        }
        return Some(out);
    }
    let mut shell_mode = false;
    // the options before the command
    while let Some(x) = args.get(i) {
        if x == "--" || !x.starts_with('-') || x == "-" {
            break;
        }
        let n = name(x);
        let step = if x.contains('=') { 1 } else { 2 };
        if r.none.contains(&n.as_str()) {
            return Some(out);
        } else if r.line.contains(&n.as_str()) {
            let l = x
                .split_once('=')
                .map(|(_, v)| v.to_string())
                .or_else(|| args.get(i + 1).cloned());
            out.extend(l.map(|l| Body::Line(l, feed.clone())));
            i += step;
        } else if r.valued.contains(&n.as_str()) {
            i += step;
        } else if r.shell.contains(&n.as_str()) {
            shell_mode = true;
            i += 1;
        } else if r.flags.contains(&n.as_str()) {
            i += 1;
        } else {
            out.push(Body::Unseen(format!(
                "{p} 的选项 {x} 不认识，看不出它运行的命令从哪开始"
            )));
            return Some(out);
        }
    }
    let mut start = if dash == Some(i) { i + 1 } else { i + r.lead };
    if args.get(start).is_some_and(|x| x == "--") {
        start += 1;
    }
    let mut words: Vec<String> = args
        .get(start..)
        .map(<[String]>::to_vec)
        .unwrap_or_default();
    if p == "cross-env" {
        // `cross-env A=1 B=2 cmd`
        let n = words.iter().take_while(|w| w.contains('=')).count();
        words.drain(..n);
    }
    if !words.is_empty() {
        out.push(if shell_mode {
            Body::Line(words.join(" "), feed.clone())
        } else {
            Body::Argv(words, feed.clone())
        });
    }
    Some(out)
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
    if args[..i].iter().any(|x| x.starts_with("--exec-path")) {
        out.push(Body::Unseen(
            "git --exec-path 让 git 从别处找子命令，看不出会运行什么".into(),
        ));
    }
    // options whose value is a command git runs: a pager, a diff tool, an upload-pack program
    let valued = |names: &[&str], short: Option<&str>| -> Vec<String> {
        let mut v = vec![];
        let mut it = rest.iter();
        while let Some(x) = it.next() {
            if names.contains(&x.as_str()) || short == Some(x.as_str()) {
                v.extend(it.next().cloned());
            } else if let Some(c) = names.iter().find_map(|n| x.strip_prefix(&format!("{n}="))) {
                v.push(c.into());
            } else if let Some(c) = short
                .and_then(|s| x.strip_prefix(s))
                .filter(|c| !c.is_empty())
            {
                v.push(c.into());
            }
        }
        v
    };
    let lines = match sub {
        "grep" => {
            // `-O` alone opens the default pager; `-Ocmd` / `--open-files-in-pager=cmd` name one
            let mut v: Vec<String> = rest
                .iter()
                .filter_map(|x| {
                    x.strip_prefix("-O")
                        .or_else(|| x.strip_prefix("--open-files-in-pager="))
                        .filter(|c| !c.is_empty())
                        .map(String::from)
                })
                .collect();
            v.retain(|c| !c.starts_with('-'));
            v
        }
        "difftool" => valued(&["--extcmd"], Some("-x")),
        "fetch" | "pull" => valued(&["--upload-pack"], None),
        "clone" | "ls-remote" => valued(&["--upload-pack"], Some("-u")),
        "archive" => valued(&["--exec"], None),
        _ => vec![],
    };
    out.extend(lines.into_iter().map(|l| Body::Line(l, feed.clone())));
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
    /// Short options whose value can only be attached, the rest of the word (`-i.bak`, `-x/dir`).
    attached: &'static str,
    /// Short options followed by digits only, if any (perl's `-l`, `-0777`); a letter after them
    /// is the next option of the cluster (`-lne`).
    digits: &'static str,
    /// Options whose value is a module loaded before the code (`--import`, `-r`): one given inline
    /// (`data:` URL) is code that cannot be read.
    loaders: &'static [&'static str],
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
        digits: "",
        loaders: &[],
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
        digits: "",
        loaders: &[
            "-r",
            "--require",
            "--import",
            "--loader",
            "--experimental-loader",
        ],
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
        digits: "",
        loaders: &["-r", "--preload"],
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
        digits: "",
        loaders: &[],
    },
    Interp {
        names: &["ruby", "irb"],
        lang: Lang::Ruby,
        code: "e",
        code_attached: true,
        long_code: NO_LONG,
        valued: "IrCEF",
        long_valued: &["--encoding", "--external-encoding", "--internal-encoding"],
        attached: "ix",
        digits: "0WT",
        loaders: &["-r"],
    },
    Interp {
        names: &["perl"],
        lang: Lang::Perl,
        code: "eE",
        code_attached: true,
        long_code: NO_LONG,
        valued: "IMmF",
        long_valued: NO_LONG,
        attached: "ixCdD",
        digits: "l0",
        loaders: &["-M", "-m"],
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
        digits: "",
        loaders: &[],
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
        digits: "",
        loaders: &[],
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
        digits: "",
        loaders: &[],
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
        digits: "",
        loaders: &[],
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
        digits: "",
        loaders: &[],
    },
];

/// An interpreter's command line, read.
pub(crate) struct Inline {
    pub lang: Lang,
    /// Every piece of code it is given (`-e` may come more than once).
    pub codes: Vec<String>,
    /// Command lines it hands to a shell (`bun exec`).
    pub lines: Vec<String>,
    /// It reads its program from standard input: no code, no module, and no script or one that is
    /// standard input (`-`, `/dev/stdin`).
    pub stdin: bool,
    /// Code it is given that cannot be read at all (a module as a `data:` URL).
    pub unseen: Option<String>,
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
        unseen: None,
    };
    let mut first: Option<&str> = None;
    let mut module = false;
    let mut i = 0;
    let mut opts = true;
    // a module given inline is code nothing here can read
    let mut load = |opt: &str, v: Option<&String>| {
        if it.loaders.contains(&opt) && v.is_some_and(|v| v.trim_start().starts_with("data:")) {
            out.unseen = Some(format!(
                "{p} {opt} 加载的是写在命令里的模块，看不出会做什么"
            ));
        }
    };
    let mut codes = vec![];
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
                Some(v) => codes.push(v.to_string()),
                None => {
                    codes.extend(args.get(i).cloned());
                    i += 1;
                }
            }
            continue;
        }
        if opts && x.starts_with("--") {
            match x.split_once('=') {
                Some((n, v)) => load(n, Some(&v.to_string())),
                None if it.long_valued.contains(&x.as_str()) => {
                    load(x, args.get(i));
                    i += 1;
                }
                None => {}
            }
            continue;
        }
        if opts && x.len() > 1 && x.starts_with('-') {
            let cs: Vec<char> = x[1..].chars().collect();
            let mut j = 0;
            while let Some(&c) = cs.get(j) {
                let rest: String = cs[j + 1..].iter().collect();
                if it.code.contains(c) {
                    if it.code_attached && !rest.is_empty() {
                        codes.push(rest);
                    } else {
                        codes.extend(args.get(i).cloned());
                        i += 1;
                    }
                    break;
                }
                if it.valued.contains(c) {
                    module |= it.lang == Lang::Python && c == 'm';
                    let opt = format!("-{c}");
                    if rest.is_empty() {
                        load(&opt, args.get(i));
                        i += 1;
                    } else {
                        load(&opt, Some(&rest));
                    }
                    break;
                }
                if it.attached.contains(c) {
                    break;
                }
                j += 1;
                if it.digits.contains(c) {
                    while cs.get(j).is_some_and(|d| d.is_ascii_digit() || *d == ':') {
                        j += 1;
                    }
                }
            }
            continue;
        }
        first.get_or_insert(x.as_str());
    }
    out.codes = codes;
    match (p, first) {
        ("deno", Some("eval")) => out
            .codes
            .extend(operands(args).get(1).map(|s| s.to_string())),
        ("deno", Some("run")) => {
            out.stdin = operands(args)
                .get(1)
                .is_none_or(|s| STDIN_PATHS.contains(&s.as_str()));
        }
        ("bun", Some("exec")) => out
            .lines
            .extend(operands(args).get(1).map(|s| s.to_string())),
        _ => {}
    }
    if p != "deno" && out.codes.is_empty() && !module {
        out.stdin = first.is_none_or(|f| STDIN_PATHS.contains(&f));
    } else if p == "deno" && first.is_none() {
        out.stdin = true;
    }
    Some(out)
}

/// Words in code, in any of the languages, that start programs, call into native code, signal a
/// process or evaluate text as code.
static RUNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(system\w*|popen\w*|spawn\w*|exec|execv\w*|execl\w*|execSync|execFile\w*|execa",
        r"|exec_command|eval|fork\w*|subprocess|child_process|posix_spawn\w*|startfile|webbrowser",
        r"|pty|ctypes|cffi|ffi|fiddle|dlopen|syscall|setuid|seteuid|setreuid|setresuid|setgid",
        r"|setegid|setgroups|initgroups|open3|shell_exec|passthru|proc_open|pcntl_\w+|posix_\w+",
        r"|kill\w*|\w+_kill|pkill|killall|sendmail|nodemailer)\b|create_subprocess",
    ))
    .expect("valid regex")
});
/// JavaScript (and PHP) comments.
static JS_COMMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/\*[\s\S]*?\*/|//[^\n]*").expect("valid regex"));
/// What reads like [`RUNS`] but only names this process or its platform.
static PLAIN_CALLS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bplatform\.system\(\)|\bsys\.executable\b").expect("valid regex")
});
/// Per language: loading code by a computed name, reaching a function by one (reflection),
/// unpickling.
static PY_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(__import__|importlib|runpy|getattr|setattr|delattr|globals|locals|vars|inspect",
        r"|attrgetter|methodcaller|pickle|cPickle|marshal|dill|shelve|jsonpickle|unsafe_load",
        r"|builtins|timeit|cProfile|pdb|bdb|doctest|pydoc|pipepager|breakpoint|runcall|runctx",
        r"|pkgutil|resolve_name|import_module|get_field|Formatter|smtplib)\b",
        r"|\byaml\.load\b|\bcode\.\w+|\bprofile\.run|\btrace\.Trace",
    ))
    .expect("valid regex")
});
static JS_DYNAMIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(Function|constructor|__proto__|Reflect|globalThis|global|vm|worker_threads|Worker",
        r"|WebAssembly|mainModule|binding|_linkedBinding|getBuiltinModule|Deno|Bun)\b",
        r"|['\x22`]\s*data:",
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
    Regex::new(r"\b(load|loadstring|loadfile|dofile|require|debug|package|os\.execute|io\.popen)\b")
        .expect("valid regex")
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
        r"`|%x|\bqx\b|['\x22]\s*-?\||\|\s*['\x22]|\breadpipe\b|\bopen2\b|IPC::|\bExpect\b",
        r"|&\s*[\{\$]|\*\s*\{|->\s*[\$\(]|\b(mail|mb_send_mail)\s*\(|Net::SMTP|net/smtp",
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
    // a comment between a name and its call (`require/**/(…)`) hides nothing; a `//` inside a
    // string is not a comment: both readings are looked at
    if matches!(lang, Lang::Js | Lang::Php) {
        let bare = JS_COMMENTS.replace_all(code, " ");
        if bare != code
            && let Some(why) = code_runs_as(lang, &bare)
        {
            return Some(why);
        }
    }
    code_runs_as(lang, code)
}

/// [`code_runs`] on one reading of the code.
fn code_runs_as(lang: Lang, code: &str) -> Option<String> {
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
    let code = &PLAIN_CALLS.replace_all(code, "");
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
            // GNU takes any unambiguous start of a long option (`--expr`, `--fi`)
            let Some(n) = SED_LONG
                .iter()
                .find(|o| **o == n)
                .or_else(|| {
                    let all: Vec<&&str> = SED_LONG.iter().filter(|o| o.starts_with(n)).collect();
                    (all.len() == 1).then(|| all[0])
                })
                .copied()
            else {
                return Some(format!("sed 的选项 {x} 不认识，看不出它的脚本是哪个"));
            };
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
                // GNU's `-l N` (line length); BSD's `-l` takes nothing
                'l' if rest.is_empty()
                    && args
                        .get(i)
                        .is_some_and(|n| n.chars().all(|d| d.is_ascii_digit())) =>
                {
                    i += 1;
                    break;
                }
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

/// GNU sed's long options.
const SED_LONG: &[&str] = &[
    "binary",
    "debug",
    "expression",
    "file",
    "follow-symlinks",
    "help",
    "in-place",
    "line-length",
    "null-data",
    "zero-terminated",
    "posix",
    "quiet",
    "silent",
    "regexp-extended",
    "sandbox",
    "separate",
    "unbuffered",
    "version",
];

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
    fn runners_commands_start_after_their_options() {
        let argv = |p: &str, a: &[&str]| -> Option<Vec<Vec<String>>> {
            sub_runner(p, &w(a), &Feed::default()).map(|bs| {
                bs.into_iter()
                    .map(|b| match b {
                        Body::Argv(v, _) => v,
                        Body::Line(l, _) => vec![format!("line:{l}")],
                        Body::Unseen(_) => vec!["unseen".into()],
                        other => vec![format!("{other:?}")],
                    })
                    .collect()
            })
        };
        assert_eq!(
            argv("npm", &["--prefix", "x", "exec", "--", "sudo", "id"]),
            Some(vec![w(&["sudo", "id"])])
        );
        assert_eq!(
            argv("npm", &["exec", "-c", "pkill x"]),
            Some(vec![w(&["line:pkill x"])])
        );
        assert_eq!(
            argv("npm", &["install", "exec"]),
            None,
            "another subcommand"
        );
        assert_eq!(argv("npm", &["test"]), None);
        assert_eq!(
            argv("uv", &["run", "--with", "x", "pytest"]),
            Some(vec![w(&["pytest"])])
        );
        assert_eq!(
            argv("uv", &["run", "-m", "pytest"]),
            Some(vec![]),
            "a module"
        );
        assert_eq!(
            argv("uv", &["run", "--frob", "x", "pkill"]),
            Some(vec![w(&["unseen"])])
        );
        assert_eq!(
            argv("conda", &["run", "-n", "env", "pkill", "x"]),
            Some(vec![w(&["pkill", "x"])])
        );
        assert_eq!(
            argv("aws-vault", &["exec", "prof", "--", "aws", "s3"]),
            Some(vec![w(&["aws", "s3"])])
        );
        assert_eq!(
            argv("direnv", &["exec", ".", "git", "push"]),
            Some(vec![w(&["git", "push"])])
        );
        assert_eq!(
            argv("mise", &["exec", "node@20", "--", "id"]),
            Some(vec![w(&["id"])])
        );
        assert_eq!(
            argv("mise", &["exec", "node@20"]),
            Some(vec![]),
            "no command: a shell"
        );
        assert_eq!(
            argv("cross-env", &["A=1", "B=2", "pkill", "x"]),
            Some(vec![w(&["pkill", "x"])])
        );
        assert_eq!(
            argv("pnpm", &["exec", "-c", "pkill", "x"]),
            Some(vec![w(&["line:pkill x"])])
        );
        assert_eq!(argv("ccache", &["-s"]), Some(vec![]));
    }

    #[test]
    fn a_shell_reads_its_script_from_standard_input() {
        for a in [
            &[][..],
            &["-s"],
            &["-s", "x"],
            &["-xs"],
            &["-"],
            &["/dev/stdin"],
            &["-o", "pipefail"],
            &["-e", "/dev/fd/0"],
        ] {
            assert!(shell_reads_stdin(&w(a)), "{a:?}");
        }
        for a in [
            &["x.sh"][..],
            &["-o", "pipefail", "x.sh"],
            &["-e", "x.sh", "-s"],
        ] {
            assert!(!shell_reads_stdin(&w(a)), "{a:?}");
        }
    }

    #[test]
    fn variables_and_options_that_carry_commands() {
        let v = var_bodies(&w(&[
            "GIT_EDITOR=git push",
            "A=1",
            "LD_PRELOAD=/x.so",
            "git",
            "commit",
            "PAGER=data",
        ]));
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(matches!(&v[0], Body::Line(l, _) if l == "git push"));
        assert!(matches!(&v[1], Body::Unseen(_)));
        assert!(var_bodies(&w(&["echo", "PATH=/x", "FOO=gcc"])).is_empty());
        let o = option_bodies(
            "fd",
            &w(&["-e", "rs", "-x", "rm", "{}", ";", "-H"]),
            &Feed::default(),
        );
        assert_eq!(o, vec![Body::Argv(w(&["rm", "{}"]), Feed::default())]);
        assert!(option_bodies("fd", &w(&["-e", "rs"]), &Feed::default()).is_empty());
        assert_eq!(
            option_bodies(
                "tar",
                &w(&["-xf", "a", "--to-command=cat"]),
                &Feed::default()
            ),
            vec![Body::Line("cat".into(), Feed::default())]
        );
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
