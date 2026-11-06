//! The tool guard: which agent permission requests YOLO answers with
//! `allow_once`. Default deny: anything it does not positively recognise as work inside the item's own
//! directory is left to the user. It runs after the hard limits, which it does not repeat.
//!
//! The user's `extra_programs` widen the program list only. Unreadable or opaque commands (command
//! substitution, `eval`, `sh -c`, `sudo`, variables, subshells), truncated input and tools of other MCP
//! servers stay with the user whatever the settings say.
//!
//! In scope (2026-11-02, after ST-21): the item's own directory — also when it lives under
//! `STEVE_HOME` — and the shared temporary directories (`/tmp`, `/private/tmp`, `$TMPDIR`), where files
//! may be created and written but not deleted. Codex's requests come as `{command: [shell, -lc,
//! script], cwd}` and `{changes: {path: {type, unified_diff, move_path}}}`; both are read. A compound
//! line is allowed when every part is. `kill <pid>` only for processes this session's agent started.
//!
//! Closed bypasses (U3/U4, 2026-11-06): inline code (`python3 -c`, `node -e`…) that mentions anything
//! outward, an awk program with `system()`, a pipe or a redirection, `npx` of a package that is not a
//! known build tool, a web fetch with a query or a long path, and a here-document fed to anything but
//! a reader all stay with the user.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::judge::{Env, Tool};
use super::limits::{Places, tool_base, tool_command, tool_kind, tool_paths, tool_targets};
use super::shell::{self, Argv};

/// The guard's answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Guard {
    /// Answer with this `allow_once` option; `checks` is what the digest shows.
    Allow { option: String, checks: Value },
    /// Leave it to the user, with the reason shown on the card.
    Ask(String),
}

fn ask(why: impl Into<String>) -> Guard {
    Guard::Ask(why.into())
}

/// Programs never run unattended, even when the user listed them.
const NEVER: &[&str] = &[
    "sudo",
    "doas",
    "su",
    "eval",
    "source",
    ".",
    "exec",
    "pkill",
    "killall",
    "launchctl",
    "osascript",
    "open",
    "security",
    "ssh",
    "scp",
    "sftp",
    "shutdown",
    "reboot",
    "crontab",
    "defaults",
    "diskutil",
    "networksetup",
    "chsh",
    "passwd",
    "dscl",
    "spctl",
    "csrutil",
    "tccutil",
];
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "ksh", "csh", "tcsh"];
/// Programs that only read (their path arguments must be readable places).
const READERS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "awk",
    "wc",
    "sort",
    "jq",
    "uniq",
    "cut",
    "tr",
    "diff",
    "cmp",
    "file",
    "stat",
    "du",
    "tree",
    "realpath",
    "basename",
    "dirname",
    "echo",
    "printf",
    "pwd",
    "true",
    "false",
    "test",
    "[",
    "which",
    "date",
    "sleep",
    "nl",
    "column",
    "less",
    "more",
    "od",
    "xxd",
    "hexdump",
    "md5",
    "shasum",
    "sha256sum",
    "comm",
    "paste",
    "fold",
    "rev",
    "tac",
    "yes",
    "seq",
    "expr",
    "type",
    "whoami",
    "uname",
    "env",
    "printenv",
    "fd",
];
/// Programs that write; every path argument must be inside the item's directory.
const WRITERS: &[&str] = &[
    "mkdir", "cp", "mv", "rm", "touch", "rmdir", "ln", "sed", "tee",
];
/// Build and test tools run as is.
const TOOLCHAIN: &[&str] = &[
    "node",
    "tsx",
    "ts-node",
    "vite",
    "python3",
    "python",
    "pytest",
    "go",
    "make",
    "tsc",
    "deno",
    "bun",
    "rustc",
    "rustfmt",
    "clippy-driver",
    "swift",
    "swiftc",
    "xcodebuild",
    "gradle",
    "mvn",
    "javac",
    "java",
    "ruby",
    "bundle",
    "uv",
    "vitest",
    "jest",
    "eslint",
    "prettier",
    "biome",
    "cmake",
    "ninja",
    "gcc",
    "clang",
    "cc",
    "ld",
];

/// Packages `npx` (and `npm exec`, `pnpm dlx`…) may run unattended: the build and test tools of a
/// project, which it runs from `node_modules`.
const NPX_TOOLS: &[&str] = &[
    "tsc",
    "tsx",
    "ts-node",
    "vite",
    "vitest",
    "jest",
    "eslint",
    "prettier",
    "biome",
    "svelte-check",
    "vue-tsc",
    "astro",
    "next",
    "nuxt",
    "webpack",
    "rollup",
    "esbuild",
    "turbo",
    "nx",
    "mocha",
    "ava",
    "c8",
    "nyc",
    "stylelint",
    "oxlint",
    "knip",
];
/// Scoped packages `npx` may run, and the program each one is.
const NPX_SCOPED: &[(&str, &str)] = &[("@biomejs/biome", "biome")];
/// Inline code that may reach outside: outward words, network and process calls.
static OUTWARD_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)推送|发布|发送|删除|部署|上线|\b(push|publish|deploy|release|send|sendmail|upload|delete|unlink|rmtree|remove|email|smtp|socket|subprocess|child_process|system|popen|spawn|exec|execsync|fetch|requests|urllib|httpx|aiohttp|axios|https?|curl|wget|ssh|xmlhttprequest|websocket)\b")
        .expect("valid regex")
});
/// A fetched URL's path longer than this may carry data out.
const FETCH_PATH_MAX: usize = 100;

/// Checks one permission request; `extra` = the user's additional programs.
pub fn check(t: &Tool, extra: &[String], env: &Env) -> Guard {
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
    let Some(cwd) = t.cwd.as_deref() else {
        return ask("这件事项没有自己的工作区");
    };
    let mut pl = Places::new(Some(cwd), env);
    let kind = tool_kind(t);
    if t.title.starts_with("mcp__") || kind == "other" {
        return ask(format!("{} 不是文件或命令操作，留给你", t.title));
    }
    let paths = tool_paths(t);
    let allow = |matched: Value, commands: Vec<String>| Guard::Allow {
        option: option.clone(),
        checks: json!({ "kind": kind, "paths": paths, "commands": commands, "matched": matched }),
    };
    match kind {
        "think" => allow(json!("think"), vec![]),
        "read" | "search" => {
            for p in &paths {
                if let Some(why) = unreadable(p, &pl, t.project_dir.as_deref()) {
                    return ask(why);
                }
            }
            allow(json!("read"), vec![])
        }
        "edit" | "delete" | "move" => {
            if paths.is_empty() {
                return ask("看不出改的是哪个文件");
            }
            if let Some(base) = tool_base(t, &pl) {
                pl.base = base;
            }
            for (p, removes) in tool_targets(t) {
                match pl.resolve(&p) {
                    Some(rp) if pl.inside(&rp) => {}
                    Some(rp) if !removes && pl.writable(&rp) => {}
                    _ => return ask(format!("{p} 不在工作区内")),
                }
            }
            allow(json!("worktree"), vec![])
        }
        "fetch" => {
            let url = t.raw.get("url").and_then(Value::as_str).unwrap_or("");
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return ask("不是网页地址");
            }
            if let Some(why) = fetch_carries(url) {
                return ask(why);
            }
            allow(json!("get"), vec![])
        }
        "execute" => {
            let Some(cmd) = tool_command(t) else {
                return ask("看不出要执行的命令");
            };
            match tool_base(t, &pl) {
                Some(Some(b)) if pl.writable(&b) => pl.base = Some(b),
                Some(_) => {
                    let d = t.raw.get("cwd").and_then(Value::as_str).unwrap_or("");
                    return ask(format!("命令的工作目录不在工作区内：{d}"));
                }
                None => {}
            }
            let cx = Cx {
                project: t.project_dir.as_deref(),
                extra,
                pids: &t.own_pids,
            };
            match command(&cmd, &pl, &cx) {
                Ok(matched) => allow(json!(matched), vec![cmd]),
                Err(why) => ask(why),
            }
        }
        other => ask(format!("不认识的工具类型 {other}")),
    }
}

/// Why fetching `url` may send data out (in its query or a long path); `None`: a plain page.
fn fetch_carries(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, q),
        None => (rest, ""),
    };
    let path = path.split_once('/').map_or("", |(_, p)| p);
    if !query.is_empty() {
        return Some("网址带查询参数，可能把数据带出去，留给你".into());
    }
    if path.chars().count() > FETCH_PATH_MAX {
        return Some("网址的路径很长，可能把数据带出去，留给你".into());
    }
    None
}

/// The inline code of an interpreter's command line (`python3 -c CODE`, `node -e CODE`, `deno eval
/// CODE`…), when there is one.
fn inline_code<'a>(p: &str, args: &'a [String]) -> Option<&'a str> {
    let flags: &[&str] = match p {
        "python" | "python3" => &["-c"],
        "node" | "bun" | "tsx" | "ts-node" => &["-e", "--eval", "-p", "--print"],
        "ruby" => &["-e"],
        "perl" => &["-e", "-E"],
        "deno" => {
            return (args.first().map(String::as_str) == Some("eval"))
                .then(|| operands(&args[1..]).first().map(|s| s.as_str()))
                .flatten();
        }
        _ => return None,
    };
    let i = args.iter().position(|a| flags.contains(&a.as_str()))?;
    args.get(i + 1).map(String::as_str)
}

/// An awk command line read the way awk reads it: the program texts (`-e`/`--source`, or the first
/// operand) and the operands after them; `None` when the program is in a file (`-f`).
fn awk_parts(args: &[String]) -> Option<(Vec<&str>, Vec<&String>)> {
    let mut progs = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let s = a.as_str();
        match s {
            "--" => {
                i += 1;
                break;
            }
            // options that take the next word as their value
            "-v" | "-F" | "--assign" | "--field-separator" => i += 2,
            "-e" | "--source" => {
                progs.push(args.get(i + 1).map_or("", String::as_str));
                i += 2;
            }
            _ if s.starts_with("-f") || s.starts_with("--file") => return None,
            _ if s.starts_with("--source=") => {
                progs.push(&s["--source=".len()..]);
                i += 1;
            }
            _ if s.starts_with('-') && s != "-" => i += 1,
            _ => break,
        }
    }
    let rest = &args[i.min(args.len())..];
    let (prog, files) = match (progs.is_empty(), rest.split_first()) {
        (true, Some((p, f))) => (Some(p.as_str()), f),
        _ => (None, rest),
    };
    progs.extend(prog);
    Some((progs, files.iter().collect()))
}

/// An interpreter that reads its program from standard input (`… | python3`, `… | node -`): no inline
/// code, no module and no script operand.
fn program_from_stdin(p: &str, args: &[String]) -> bool {
    let valued: &[&str] = match p {
        "python" | "python3" => &["-W", "-X", "-Q"],
        "node" | "bun" | "tsx" | "ts-node" => &[
            "-r",
            "--require",
            "--import",
            "-C",
            "--conditions",
            "--loader",
            "--experimental-loader",
            "--input-type",
        ],
        "ruby" => &["-I", "-r", "-C", "-E", "-F"],
        "perl" => &["-I", "-M", "-m"],
        "deno" => {
            let ops = operands(args);
            return match ops.first().map(|s| s.as_str()) {
                None | Some("-") => true,
                Some("run") => ops.get(1).is_none_or(|s| *s == "-"),
                _ => false,
            };
        }
        _ => return false,
    };
    if inline_code(p, args).is_some()
        || matches!(p, "python" | "python3") && args.iter().any(|a| a == "-m")
    {
        return false;
    }
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-" => return true,
            "--" => return args.get(i + 1).is_none_or(|s| s == "-"),
            s if valued.contains(&s) => i += 2,
            s if s.starts_with('-') => i += 1,
            _ => return false,
        }
    }
    true
}

/// Why an awk program is more than a reader: it runs commands (`system()`, a pipe) or writes files.
fn awk_runs(args: &[String]) -> Option<String> {
    let Some((progs, _)) = awk_parts(args) else {
        return Some("awk 的程序在文件里，看不出会做什么".into());
    };
    static WRITES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(print|printf)[^;{}]*>"#).expect("valid regex"));
    if progs
        .iter()
        .any(|p| p.contains("system") || p.contains('|') || WRITES.is_match(p))
    {
        return Some("awk 程序里有 system()、管道或写文件，会执行或改别的东西".into());
    }
    None
}

/// Why `p` may not be read unattended; `None` = it may.
fn unreadable(p: &str, pl: &Places, project: Option<&Path>) -> Option<String> {
    let Some(rp) = pl.resolve(p) else {
        return Some(format!("看不出 {p} 指向哪里"));
    };
    let home = &pl.home;
    let secret = [
        ".ssh",
        ".aws",
        ".gnupg",
        ".config/gh",
        "Library/Keychains",
        ".netrc",
        ".npmrc",
    ]
    .iter()
    .any(|s| rp.starts_with(home.join(s)));
    if secret {
        return Some(format!("{p} 是密钥或凭据目录"));
    }
    if pl.writable(&rp) {
        return None;
    }
    let name = rp
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.starts_with(".env") {
        return Some(format!("{p} 是工作区外的环境变量文件"));
    }
    let toolchain: [PathBuf; 4] = [
        PathBuf::from("/usr"),
        PathBuf::from("/opt/homebrew"),
        home.join(".cargo/registry"),
        home.join(".rustup"),
    ];
    let ok = project.is_some_and(|d| rp.starts_with(super::limits::canonical(d)))
        || toolchain.iter().any(|d| rp.starts_with(d))
        || rp.components().any(|c| c.as_os_str() == "node_modules");
    if ok {
        None
    } else {
        Some(format!("{p} 在工作区外"))
    }
}

/// What a command is judged against besides the places.
struct Cx<'a> {
    /// The project's main directory (readable).
    project: Option<&'a Path>,
    /// The user's additional programs.
    extra: &'a [String],
    /// Processes this session's agent started (may be stopped by PID).
    pids: &'a [u32],
}

/// The command is allowed: returns the programs it matched; or why not. Every part of a compound line
/// (`&&`, `;`, `|`, `X=y cmd`, redirections) must be allowed on its own.
fn command(cmd: &str, pl: &Places, cx: &Cx) -> Result<Vec<String>, String> {
    let p = shell::parse(cmd).map_err(|e| format!("命令看不懂：{}", e.0))?;
    if let Some(what) = p.opaque.first() {
        return Err(format!("命令里有 {what}，看不出实际会执行什么"));
    }
    if p.cmds.is_empty() {
        return Err("空命令".into());
    }
    let mut pl = pl.clone();
    let mut matched = Vec::new();
    for c in &p.cmds {
        if c.expands {
            return Err("命令里有变量展开，看不出实际的值".into());
        }
        for w in &c.writes {
            if !matches!(w.as_str(), "/dev/null" | "/dev/stdout" | "/dev/stderr") {
                match pl.resolve(w) {
                    Some(rp) if pl.writable(&rp) => {}
                    _ => return Err(format!("输出写到工作区外：{w}")),
                }
            }
        }
        let Some(a) = shell::argv(&c.words) else {
            continue;
        };
        if a.sudo {
            return Err("sudo 留给你".into());
        }
        // a here-document is text for a reader (`cat <<EOF > file`), not a script for anything else
        if c.stdin.is_some() && !(READERS.contains(&a.program.as_str()) || a.program == "tee") {
            return Err(format!(
                "here-document 喂给 {}，看不出会执行什么",
                a.program
            ));
        }
        if c.piped && program_from_stdin(&a.program, &a.args) {
            return Err(format!("管道把程序喂给 {}，看不出会执行什么", a.program));
        }
        if matches!(a.program.as_str(), "cd" | "pushd") {
            let target = a.args.first().map(String::as_str).unwrap_or("~");
            match pl.resolve(target) {
                Some(rp) if pl.writable(&rp) => pl.base = Some(rp),
                _ => return Err(format!("cd 到工作区外：{target}")),
            }
            continue;
        }
        simple(&a, &pl, cx)?;
        matched.push(a.program.clone());
    }
    Ok(matched)
}

fn operands(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut opts = true;
    for a in args {
        if opts && a == "--" {
            opts = false;
            continue;
        }
        if opts && a.starts_with('-') && a != "-" {
            continue;
        }
        out.push(a);
    }
    out
}

fn simple(a: &Argv, pl: &Places, cx: &Cx) -> Result<(), String> {
    let (project, extra) = (cx.project, cx.extra);
    let p = a.program.as_str();
    let args = &a.args;
    if p == "kill" {
        return kill(args, cx.pids);
    }
    if NEVER.contains(&p) {
        return Err(format!("{p} 留给你"));
    }
    if SHELLS.contains(&p) {
        return Err(format!("{p} 会执行看不见的命令"));
    }
    if a.xargs {
        return Err("xargs 的参数来自标准输入，看不出".into());
    }
    if let Some(code) = inline_code(p, args)
        && let Some(m) = OUTWARD_CODE.find(code)
    {
        return Err(format!(
            "内联代码里有对外或执行别的程序的字眼（{}），留给你",
            m.as_str()
        ));
    }
    let sub = operands(args).first().map(|s| s.as_str()).unwrap_or("");
    let inside_all = |list: Vec<&String>| -> Result<(), String> {
        for w in list {
            match pl.resolve(w) {
                Some(rp) if pl.inside(&rp) => {}
                _ => return Err(format!("{p} 的路径不在工作区内：{w}")),
            }
        }
        Ok(())
    };
    let writable_all = |list: Vec<&String>| -> Result<(), String> {
        for w in list {
            match pl.resolve(w) {
                Some(rp) if pl.writable(&rp) => {}
                _ => return Err(format!("{p} 的路径不在工作区内：{w}")),
            }
        }
        Ok(())
    };
    let readable_all = |list: Vec<&String>| -> Result<(), String> {
        for w in list {
            if let Some(why) = unreadable(w, pl, project) {
                return Err(why);
            }
        }
        Ok(())
    };
    match p {
        "cargo" => {
            if matches!(
                sub,
                "install" | "uninstall" | "login" | "logout" | "publish" | "yank" | "owner"
            ) {
                return Err(format!("cargo {sub} 留给你"));
            }
            Ok(())
        }
        "npm" | "pnpm" | "yarn" => {
            if args.iter().any(|x| x == "-g" || x == "--global") {
                return Err("全局安装留给你".into());
            }
            let ok = matches!(
                sub,
                "run"
                    | "run-script"
                    | "test"
                    | "t"
                    | "build"
                    | "install"
                    | "i"
                    | "ci"
                    | "exec"
                    | ""
            ) || p == "yarn" && !matches!(sub, "global" | "publish" | "npm" | "dlx");
            if !ok {
                return Err(format!("{p} {sub} 留给你"));
            }
            if sub == "exec" {
                // `npm exec X` runs package X like npx
                let i = args.iter().position(|x| x == "exec").unwrap_or(0);
                return npx(&args[i + 1..], pl, cx);
            }
            Ok(())
        }
        "npx" | "pnpx" | "bunx" => npx(args, pl, cx),
        "ps" | "pgrep" | "lsof" => Ok(()),
        "go" => {
            if matches!(sub, "install" | "env" | "get") {
                Err(format!("go {sub} 会改工作区以外"))
            } else {
                Ok(())
            }
        }
        "python3" | "python" => {
            if args.windows(2).any(|w| {
                w[0] == "-m" && matches!(w[1].as_str(), "pip" | "ensurepip" | "venv" | "twine")
            }) {
                Err("pip / venv 留给你".into())
            } else {
                Ok(())
            }
        }
        "git" => git(args, pl, extra),
        "find" => {
            if args.iter().any(|x| {
                matches!(
                    x.as_str(),
                    "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir" | "-fprint" | "-fls"
                )
            }) {
                return Err("find 的 -delete / -exec 留给你".into());
            }
            let roots: Vec<&String> = args
                .iter()
                .take_while(|x| !x.starts_with('-') && x.as_str() != "(" && x.as_str() != "!")
                .collect();
            readable_all(roots)
        }
        "sed" => {
            if args
                .iter()
                .any(|x| x.starts_with("-i") || x.starts_with("--in-place"))
            {
                // the script is the first operand unless given with -e/-f
                let script_opt = args.iter().any(|x| x == "-e" || x == "-f");
                let ops = operands(args);
                writable_all(if script_opt {
                    ops
                } else {
                    ops.into_iter().skip(1).collect()
                })
            } else {
                readable_all(operands(args).into_iter().skip(1).collect())
            }
        }
        "rm" | "rmdir" | "mv" => inside_all(operands(args)),
        "cp" => {
            // sources are read, the destination written
            let ops = operands(args);
            let Some((dest, srcs)) = ops.split_last() else {
                return Ok(());
            };
            readable_all(srcs.to_vec())?;
            writable_all(vec![*dest])
        }
        _ if WRITERS.contains(&p) => writable_all(operands(args)),
        "curl" => curl_local(args, pl),
        _ if READERS.contains(&p) => {
            if p == "awk"
                && let Some(why) = awk_runs(args)
            {
                return Err(why);
            }
            if p == "awk" {
                let files = awk_parts(args).map(|(_, f)| f).unwrap_or_default();
                return readable_all(files);
            }
            // grep/rg/jq take a pattern first
            let skip = usize::from(matches!(p, "grep" | "egrep" | "fgrep" | "rg" | "jq"));
            readable_all(operands(args).into_iter().skip(skip).collect())
        }
        _ if TOOLCHAIN.contains(&p) => Ok(()),
        _ if extra.iter().any(|x| x == p) => Ok(()),
        _ => Err(format!("没见过的程序：{p}")),
    }
}

/// `npx <package> …`: a known build or test tool (or one the user listed) runs as that program would;
/// any other package may be downloaded and run, so it stays with the user.
fn npx(args: &[String], pl: &Places, cx: &Cx) -> Result<(), String> {
    // npx's own options come first (`-y`, `-p <pkg>`, `--`); the package's arguments stay as they are
    let mut i = 0;
    while let Some(x) = args.get(i) {
        match x.as_str() {
            "--" => {
                i += 1;
                break;
            }
            "-p" | "--package" | "-c" | "--call" => {
                return Err("npx -p / -c 留给你".into());
            }
            s if s.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let ops = &args[i.min(args.len())..];
    let Some(pkg) = ops.first() else {
        return Err("npx 没有给要运行的包".into());
    };
    // `name@1.2` → `name`, `@scope/name@1.2` → `@scope/name`; a scope is someone's own packages, so
    // a known tool's name under another scope (`@x/eslint`) is a different package
    let spec = match pkg.strip_prefix('@') {
        Some(rest) => pkg.len() - rest.len() + rest.find('@').unwrap_or(rest.len()),
        None => pkg.find('@').unwrap_or(pkg.len()),
    };
    let spec = &pkg[..spec];
    let listed = |n: &str| cx.extra.iter().any(|x| x == n);
    let name = match NPX_SCOPED.iter().find(|(s, _)| *s == spec) {
        Some((_, n)) => *n,
        // a path, a URL or `github:user/repo` is not a package name at all
        None if spec.contains(['/', ':', '\\']) || spec.starts_with('.') => {
            if !listed(spec) {
                return Err(format!("npx 会下载并运行 {pkg}，留给你"));
            }
            spec
        }
        None => spec,
    };
    if !(NPX_TOOLS.contains(&name) || listed(name) || listed(spec)) {
        return Err(format!("npx 会下载并运行 {pkg}，留给你"));
    }
    let a = Argv {
        program: name.to_string(),
        args: ops[1..].to_vec(),
        sudo: false,
        xargs: false,
    };
    if TOOLCHAIN.contains(&name) || READERS.contains(&name) {
        simple(&a, pl, cx)
    } else {
        // a JavaScript build tool: what it is handed inline is still read
        match inline_code("node", &a.args).and_then(|c| OUTWARD_CODE.find(c)) {
            Some(m) => Err(format!(
                "内联代码里有对外或执行别的程序的字眼（{}），留给你",
                m.as_str()
            )),
            None => Ok(()),
        }
    }
}

/// `kill [-SIG] <pid>…`: only processes this session's agent started (looked up when the request
/// came); never a process group, `-1` or a job.
fn kill(args: &[String], pids: &[u32]) -> Result<(), String> {
    let mut targets = Vec::new();
    let mut i = 0;
    let mut opts = true;
    while let Some(x) = args.get(i) {
        i += 1;
        if opts && x == "--" {
            opts = false;
        } else if opts && (x == "-s" || x == "-n") {
            i += 1;
        } else if opts && (x == "-l" || x == "-L") {
            return Ok(());
        } else if opts && is_signal(x) {
        } else {
            targets.push(x.as_str());
        }
    }
    if targets.is_empty() {
        return Err("kill 没有给 PID".into());
    }
    for t in targets {
        match t.parse::<u32>() {
            Ok(pid) if pid > 1 && pids.contains(&pid) => {}
            Ok(pid) if pid > 1 => {
                return Err(format!("进程 {pid} 不是这个会话的 Agent 启动的，留给你"));
            }
            _ => return Err(format!("kill {t} 留给你")),
        }
    }
    Ok(())
}

/// `-9`, `-15`, `-TERM`, `-SIGKILL` (a longer number would be a process group).
fn is_signal(x: &str) -> bool {
    let Some(s) = x.strip_prefix('-') else {
        return false;
    };
    (!s.is_empty() && s.len() <= 2 && s.chars().all(|c| c.is_ascii_digit()))
        || (!s.is_empty() && s.chars().all(|c| c.is_ascii_uppercase()))
}

/// curl to this machine only (`127.0.0.1`, `localhost`, `[::1]`); `-o` must land in a writable place.
fn curl_local(args: &[String], pl: &Places) -> Result<(), String> {
    let Some(c) = super::limits::curl_loopback(args) else {
        return Err("curl 访问本机以外，或有看不懂的参数，留给你".into());
    };
    for o in &c.outputs {
        if o != "-" && o != "/dev/null" {
            match pl.resolve(o) {
                Some(rp) if pl.writable(&rp) => {}
                _ => return Err(format!("curl 写到工作区外：{o}")),
            }
        }
    }
    Ok(())
}

/// git: the everyday local subcommands, inside the item's directory.
fn git(args: &[String], pl: &Places, extra: &[String]) -> Result<(), String> {
    let mut i = 0;
    while let Some(x) = args.get(i) {
        match x.as_str() {
            "-C" => {
                let d = args.get(i + 1).map(String::as_str).unwrap_or("");
                match pl.resolve(d) {
                    Some(rp) if pl.inside(&rp) => {}
                    _ => return Err(format!("git -C 到工作区外：{d}")),
                }
                i += 2;
            }
            "-c" => return Err("git -c 留给你".into()),
            s if s.starts_with("--git-dir") || s.starts_with("--work-tree") => {
                return Err("git --git-dir / --work-tree 留给你".into());
            }
            s if s.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let sub = args.get(i).map(String::as_str).unwrap_or("");
    let rest = &args[(i + 1).min(args.len())..];
    if extra.iter().any(|x| x == "git") {
        return Ok(());
    }
    let ok = match sub {
        "status" | "diff" | "log" | "show" | "add" | "commit" | "checkout" | "switch" | "stash"
        | "rev-parse" | "ls-files" | "blame" | "grep" | "show-ref" | "describe" | "rev-list"
        | "merge-base" | "cat-file" | "shortlog" | "reflog" | "restore" | "mv" | "rm" => true,
        // local only: reads of the repository, and changes that stay in the item's own checkout
        "ls-tree" | "for-each-ref" | "name-rev" | "check-ignore" | "diff-tree" | "diff-index"
        | "diff-files" | "count-objects" | "var" | "version" | "help" | "cherry" | "range-diff"
        | "whatchanged" | "cherry-pick" | "revert" | "apply" => true,
        // `git bisect run CMD` runs any program on every step
        "bisect" => rest.first().is_none_or(|x| {
            matches!(
                x.as_str(),
                "start"
                    | "good"
                    | "bad"
                    | "new"
                    | "old"
                    | "skip"
                    | "reset"
                    | "log"
                    | "visualize"
                    | "view"
                    | "terms"
                    | "help"
            )
        }),
        "tag" => !rest
            .iter()
            .any(|x| x == "-d" || x == "--delete" || x == "-f" || x == "--force"),
        "worktree" => rest.first().is_some_and(|x| x == "list"),
        "reset" => !rest
            .iter()
            .any(|x| matches!(x.as_str(), "--hard" | "--merge" | "--keep")),
        "branch" => !rest
            .iter()
            .any(|x| x == "-D" || x == "-d" || x == "--delete" || x == "-m" || x == "-M"),
        "rebase" | "merge" => {
            let ops = operands(rest);
            rest.iter()
                .any(|x| matches!(x.as_str(), "--continue" | "--abort" | "--skip"))
                || !ops.is_empty()
                    && ops.iter().all(|t| {
                        matches!(
                            t.as_str(),
                            "main" | "master" | "origin/main" | "origin/master"
                        )
                    })
                    && !rest
                        .iter()
                        .any(|x| x.starts_with("-x") || x.starts_with("--exec"))
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("git {sub} 留给你"))
    }
}
