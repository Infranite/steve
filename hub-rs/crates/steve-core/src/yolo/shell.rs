//! A small POSIX-shell reader for the tool guard and the hard limits: enough to split a command line into
//! simple commands, words and redirections, and to notice the constructs whose effect cannot be read off
//! the text (command substitution, `eval`, here-strings…). It never runs anything. A here-document's
//! body is read as the command's standard input ([`Simple::stdin`]).
//!
//! It errs on the side of seeing too much: an operator inside a quoted argument is not a separator, but
//! [`nested`] lets callers parse quoted arguments as commands of their own (`sh -c '…'`, `find -exec`).

/// One simple command: its words (quotes removed) and where its output goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Simple {
    pub words: Vec<String>,
    /// Targets of `>`, `>>`, `&>`, `>|` (not fd duplications like `2>&1`).
    pub writes: Vec<String>,
    /// Words that contained an unquoted or double-quoted `$` (their value is not known here).
    pub expands: bool,
    /// The body of a here-document (`<<EOF` … `EOF`) fed to the command, as written.
    pub stdin: Option<String>,
    /// Its standard input is the output of the command before it (`a | b`, `a |& b`).
    pub piped: bool,
}

/// What [`parse`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub cmds: Vec<Simple>,
    /// Constructs the guard never lets through, in the order found: `$(…)`, backquotes, `<(…)`, a
    /// subshell, a here-string, a background `&`.
    pub opaque: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub &'static str);

#[derive(Default)]
struct Builder {
    out: Parsed,
    cur: Simple,
    word: String,
    /// The current word exists even if empty (`''`).
    in_word: bool,
    /// The next finished word is a redirection target.
    redirect: bool,
    /// A redirection without a target was seen.
    dangling: bool,
    /// Here-documents whose body starts at the next newline: (command index, delimiter, delimiter
    /// quoted, `<<-`).
    heredocs: Vec<(usize, String, bool, bool)>,
}

impl Builder {
    fn flag(&mut self, what: &'static str) {
        if !self.out.opaque.contains(&what) {
            self.out.opaque.push(what);
        }
    }

    fn end_word(&mut self) {
        if !self.in_word {
            return;
        }
        let w = std::mem::take(&mut self.word);
        self.in_word = false;
        if self.redirect {
            self.redirect = false;
            self.cur.writes.push(w);
        } else {
            self.cur.words.push(w);
        }
    }

    fn end_cmd(&mut self) {
        self.end_word();
        if self.redirect {
            self.redirect = false;
            self.dangling = true;
        }
        let c = std::mem::take(&mut self.cur);
        if !c.words.is_empty() || !c.writes.is_empty() {
            self.out.cmds.push(c);
        } else {
            // `a |` then a newline: the pipe still feeds the next command
            self.cur.piped = c.piped;
        }
    }

    fn push(&mut self, c: char) {
        self.word.push(c);
        self.in_word = true;
    }
}

/// Splits `src` on `;`, `&&`, `||`, `|`, `&`, newlines and parentheses into simple commands.
pub fn parse(src: &str) -> Result<Parsed, ParseError> {
    let mut b = Builder::default();
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            '\\' => {
                // a backslash-newline joins lines; any other escaped char is literal
                match cs.get(i + 1) {
                    Some('\n') => {}
                    Some(&n) => b.push(n),
                    None => return Err(ParseError("命令以反斜杠结尾")),
                }
                i += 2;
                continue;
            }
            '\'' => {
                b.in_word = true;
                i += 1;
                loop {
                    match cs.get(i) {
                        Some('\'') => break,
                        Some(&ch) => b.word.push(ch),
                        None => return Err(ParseError("引号没有闭合")),
                    }
                    i += 1;
                }
            }
            '"' => {
                b.in_word = true;
                i += 1;
                loop {
                    match cs.get(i) {
                        Some('"') => break,
                        Some('\\') => {
                            match cs.get(i + 1) {
                                Some(&n @ ('"' | '\\' | '$' | '`')) => b.word.push(n),
                                Some('\n') => {}
                                Some(&n) => {
                                    b.word.push('\\');
                                    b.word.push(n);
                                }
                                None => return Err(ParseError("引号没有闭合")),
                            }
                            i += 1;
                        }
                        Some('$') => {
                            b.cur.expands = true;
                            if cs.get(i + 1) == Some(&'(') {
                                b.flag("$(…)");
                            }
                            b.word.push('$');
                        }
                        Some('`') => {
                            b.flag("`…`");
                            b.word.push('`');
                        }
                        Some(&ch) => b.word.push(ch),
                        None => return Err(ParseError("引号没有闭合")),
                    }
                    i += 1;
                }
            }
            '$' => {
                b.cur.expands = true;
                if cs.get(i + 1) == Some(&'(') {
                    b.flag("$(…)");
                }
                b.push('$');
            }
            '`' => {
                b.flag("`…`");
                b.push('`');
            }
            '#' if !b.in_word => {
                // a comment runs to the end of the line
                while i < cs.len() && cs[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            ' ' | '\t' | '\r' => b.end_word(),
            '\n' if !b.heredocs.is_empty() => {
                b.end_cmd();
                i = heredoc_bodies(&mut b, &cs, i + 1)?;
                continue;
            }
            '\n' | ';' => b.end_cmd(),
            '&' => {
                if cs.get(i + 1) == Some(&'&') {
                    i += 1;
                } else if cs.get(i + 1) == Some(&'>') {
                    // `&>file`: stdout and stderr to a file
                    b.end_word();
                    b.redirect = true;
                    i += 2;
                    if cs.get(i) == Some(&'>') {
                        i += 1;
                    }
                    continue;
                } else {
                    b.flag("&");
                }
                b.end_cmd();
            }
            '|' => {
                let pipe = cs.get(i + 1) != Some(&'|');
                if matches!(cs.get(i + 1), Some('|' | '&')) {
                    i += 1;
                }
                b.end_cmd();
                b.cur.piped = pipe;
            }
            '(' | ')' => {
                b.flag("(…)");
                b.end_cmd();
            }
            '<' | '>' => {
                if c == '<' && cs.get(i + 1) == Some(&'(')
                    || c == '>' && cs.get(i + 1) == Some(&'(')
                {
                    b.flag("<(…)");
                    b.end_cmd();
                    i += 2;
                    continue;
                }
                // a word of digits right before is the file descriptor, not an argument
                if b.in_word && b.word.chars().all(|d| d.is_ascii_digit()) {
                    b.word.clear();
                    b.in_word = false;
                } else {
                    b.end_word();
                }
                if c == '<' {
                    if cs.get(i + 1) == Some(&'<') {
                        if cs.get(i + 2) == Some(&'<') {
                            // a here-string: its word is the input, expanded or not
                            b.flag("<<<");
                            b.end_cmd();
                            return Ok(b.out);
                        }
                        i += 2;
                        let strip = cs.get(i) == Some(&'-');
                        if strip {
                            i += 1;
                        }
                        while matches!(cs.get(i), Some(' ' | '\t')) {
                            i += 1;
                        }
                        let mut delim = String::new();
                        let mut quoted = false;
                        while let Some(&ch) = cs.get(i) {
                            if " \t\n;&|()<>".contains(ch) {
                                break;
                            }
                            if matches!(ch, '\'' | '"' | '\\') {
                                quoted = true;
                            } else {
                                delim.push(ch);
                            }
                            i += 1;
                        }
                        if delim.is_empty() {
                            return Err(ParseError("here-document 缺少结束标记"));
                        }
                        // the command it feeds is the one being read now
                        b.heredocs.push((b.out.cmds.len(), delim, quoted, strip));
                        continue;
                    }
                    // input redirection: the next word is read, not written
                    i += 1;
                    while matches!(cs.get(i), Some(' ' | '\t')) {
                        i += 1;
                    }
                    let mut target = String::new();
                    while let Some(&ch) = cs.get(i) {
                        if " \t\n;&|()<>".contains(ch) {
                            break;
                        }
                        target.push(ch);
                        i += 1;
                    }
                    continue;
                }
                i += 1;
                if matches!(cs.get(i), Some('>' | '|')) {
                    i += 1;
                }
                if cs.get(i) == Some(&'&') {
                    // `>&2`, `2>&1`, `>&-`: duplication, no file
                    i += 1;
                    while matches!(cs.get(i), Some(d) if d.is_ascii_digit() || *d == '-') {
                        i += 1;
                    }
                    continue;
                }
                b.redirect = true;
                continue;
            }
            _ => b.push(c),
        }
        i += 1;
    }
    b.end_cmd();
    if !b.heredocs.is_empty() {
        return Err(ParseError("here-document 没有结束"));
    }
    if b.dangling {
        return Err(ParseError("重定向缺少目标"));
    }
    Ok(b.out)
}

/// Reads the bodies of the pending here-documents from `i` (the start of the line after their
/// command), each up to its delimiter line; the index after the last one. An unquoted delimiter
/// lets the shell expand the body: a command substitution there is opaque.
fn heredoc_bodies(b: &mut Builder, cs: &[char], mut i: usize) -> Result<usize, ParseError> {
    for (idx, delim, quoted, strip) in std::mem::take(&mut b.heredocs) {
        let mut body = String::new();
        loop {
            if i >= cs.len() {
                return Err(ParseError("here-document 没有结束"));
            }
            let end = cs[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(cs.len(), |e| i + e);
            let line: String = cs[i..end].iter().collect();
            i = (end + 1).min(cs.len() + 1);
            let shown = if strip {
                line.trim_start_matches('\t')
            } else {
                line.as_str()
            };
            if shown == delim {
                break;
            }
            body.push_str(shown);
            body.push('\n');
        }
        if !quoted {
            if body.contains("$(") {
                b.flag("$(…)");
            }
            if body.contains('`') {
                b.flag("`…`");
            }
        }
        if let Some(c) = b.out.cmds.get_mut(idx) {
            c.stdin = Some(body);
        }
    }
    Ok(i.min(cs.len()))
}

/// Shell keywords that run the rest of the words as a command.
const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}",
];

/// The command a simple command really runs: leading `VAR=value` assignments, keywords and wrappers
/// (`env`, `sudo`, `nohup`, `nice`, `timeout`, `command`, `exec`, `xargs`…) removed, the program reduced
/// to its file name. `None` when only assignments remain, or a wrapper other than `sudo`/`doas` is
/// left without a command.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Argv {
    pub program: String,
    pub args: Vec<String>,
    /// Ran through `sudo`/`doas`; alone (`sudo -i`, `sudo -s`) they are the program: a root shell.
    pub sudo: bool,
    /// Ran through `xargs`: more arguments come from standard input.
    pub xargs: bool,
    /// A wrapper whose words could not be read (an option it is not known to take, an `env -S`
    /// string that is more than words): where its command starts, and so what runs, is not known.
    /// `program` and `args` are then that wrapper's.
    pub unclear: Option<String>,
}

/// How a wrapper takes its options before the command it runs. Its command starts after them (and
/// after `lead` more words); an option not listed here leaves that start unknown
/// ([`Argv::unclear`]) rather than guessed.
pub(crate) struct Wrapper {
    pub names: &'static [&'static str],
    /// Short options without a value.
    pub flags: &'static str,
    /// Short options with a value, attached (`-n5`) or the next word.
    pub valued: &'static str,
    /// Short options whose value can only be attached (`xargs -i{}`).
    pub attached: &'static str,
    /// Long options without a separate value (`--x` or `--x=v`), and with one (`--x v` or `--x=v`).
    pub long_flags: &'static [&'static str],
    pub long_valued: &'static [&'static str],
    /// `-5` is an option (nice's old form).
    pub numeric: bool,
    /// Words between the options and the command (`timeout`'s duration, `taskset`'s mask).
    pub lead: usize,
}

pub(crate) const NO_LONG: &[&str] = &[];
pub(crate) const HELP: &[&str] = &["--help", "--version"];

/// The wrappers [`argv`] sees through, as GNU and BSD document them.
const WRAPPERS: &[Wrapper] = &[
    Wrapper {
        names: &["env"],
        flags: "i0v",
        valued: "uCSP",
        attached: "",
        long_flags: &[
            "--ignore-environment",
            "--null",
            "--debug",
            "--list-signal-handling",
            "--block-signal",
            "--default-signal",
            "--ignore-signal",
            "--help",
            "--version",
        ],
        long_valued: &["--unset", "--chdir", "--split-string"],
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["sudo"],
        flags: "AbEeHiKklnPSsVvBN",
        valued: "CDghpRrTtUu",
        attached: "",
        long_flags: &[
            "--askpass",
            "--background",
            "--bell",
            "--preserve-env",
            "--edit",
            "--set-home",
            "--login",
            "--remove-timestamp",
            "--reset-timestamp",
            "--list",
            "--non-interactive",
            "--preserve-groups",
            "--stdin",
            "--shell",
            "--validate",
            "--help",
            "--version",
        ],
        long_valued: &[
            "--close-from",
            "--chdir",
            "--group",
            "--host",
            "--prompt",
            "--chroot",
            "--role",
            "--type",
            "--command-timeout",
            "--other-user",
            "--user",
        ],
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["doas"],
        flags: "nsL",
        valued: "Cu",
        attached: "",
        long_flags: NO_LONG,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["nohup", "builtin"],
        flags: "",
        valued: "",
        attached: "",
        long_flags: HELP,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["command"],
        flags: "pvV",
        valued: "",
        attached: "",
        long_flags: NO_LONG,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["exec"],
        flags: "cl",
        valued: "a",
        attached: "",
        long_flags: NO_LONG,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["caffeinate"],
        flags: "dimsu",
        valued: "tw",
        attached: "",
        long_flags: NO_LONG,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["stdbuf"],
        flags: "",
        valued: "ioe",
        attached: "",
        long_flags: HELP,
        long_valued: &["--input", "--output", "--error"],
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["unbuffer"],
        flags: "p",
        valued: "",
        attached: "",
        long_flags: NO_LONG,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["chronic"],
        flags: "ev",
        valued: "",
        attached: "",
        long_flags: NO_LONG,
        long_valued: NO_LONG,
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["ionice"],
        flags: "t",
        valued: "cnpPu",
        attached: "",
        long_flags: &["--ignore", "--help", "--version"],
        long_valued: &["--class", "--classdata", "--pid", "--pgid", "--uid"],
        numeric: false,
        lead: 0,
    },
    Wrapper {
        names: &["taskset"],
        flags: "acp",
        valued: "",
        attached: "",
        long_flags: &["--all-tasks", "--cpu-list", "--pid", "--help", "--version"],
        long_valued: NO_LONG,
        numeric: false,
        lead: 1,
    },
    Wrapper {
        names: &["nice"],
        flags: "",
        valued: "n",
        attached: "",
        long_flags: HELP,
        long_valued: &["--adjustment"],
        numeric: true,
        lead: 0,
    },
    Wrapper {
        names: &["timeout", "gtimeout"],
        flags: "v",
        valued: "sk",
        attached: "",
        long_flags: &[
            "--preserve-status",
            "--foreground",
            "--verbose",
            "--help",
            "--version",
        ],
        long_valued: &["--signal", "--kill-after"],
        numeric: false,
        lead: 1,
    },
    Wrapper {
        names: &["xargs"],
        flags: "0oprtx",
        valued: "adEILnPsJRS",
        attached: "eil",
        long_flags: &[
            "--null",
            "--open-tty",
            "--interactive",
            "--no-run-if-empty",
            "--verbose",
            "--exit",
            "--show-limits",
            "--replace",
            "--eof",
            "--help",
            "--version",
        ],
        long_valued: &[
            "--arg-file",
            "--delimiter",
            "--max-lines",
            "--max-args",
            "--max-procs",
            "--max-chars",
            "--process-slot-var",
        ],
        numeric: false,
        lead: 0,
    },
    // bash's keyword and the program (GNU's and BSD's)
    Wrapper {
        names: &["time"],
        flags: "palvhqV",
        valued: "fo",
        attached: "",
        long_flags: &[
            "--portability",
            "--verbose",
            "--append",
            "--quiet",
            "--help",
            "--version",
        ],
        long_valued: &["--format", "--output"],
        numeric: false,
        lead: 0,
    },
];

/// An option as read: its name (`-S`, `--split-string`) and value.
pub(crate) type Opt = (String, Option<String>);

/// `w`'s options from `words[i]` on: the index after them, and the options; or why they cannot be
/// read.
pub(crate) fn options(
    w: &Wrapper,
    words: &[String],
    mut i: usize,
) -> Result<(usize, Vec<Opt>), String> {
    let name = w.names[0];
    let mut out = Vec::new();
    while let Some(o) = words.get(i) {
        if o == "--" {
            i += 1;
            break;
        }
        if !o.starts_with('-') || o == "-" {
            break;
        }
        if let Some(long) = o.strip_prefix("--") {
            let (n, v) = match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (o.clone(), None),
            };
            if w.long_valued.contains(&n.as_str()) && v.is_none() {
                out.push((n, words.get(i + 1).cloned()));
                i += 2;
            } else if w.long_valued.contains(&n.as_str()) || w.long_flags.contains(&n.as_str()) {
                out.push((n, v));
                i += 1;
            } else {
                return Err(format!("{name} 的选项 {o} 不认识"));
            }
            continue;
        }
        let cs: Vec<char> = o[1..].chars().collect();
        if w.numeric && cs.iter().all(char::is_ascii_digit) {
            out.push(("-n".into(), Some(o[1..].to_string())));
            i += 1;
            continue;
        }
        let mut step = 1;
        for (j, &c) in cs.iter().enumerate() {
            let rest: String = cs[j + 1..].iter().collect();
            if w.valued.contains(c) {
                if rest.is_empty() {
                    out.push((format!("-{c}"), words.get(i + 1).cloned()));
                    step = 2;
                } else {
                    out.push((format!("-{c}"), Some(rest)));
                }
                break;
            }
            if w.attached.contains(c) {
                out.push((format!("-{c}"), (!rest.is_empty()).then_some(rest)));
                break;
            }
            if !w.flags.contains(c) {
                return Err(format!("{name} 的选项 {o} 不认识"));
            }
            out.push((format!("-{c}"), None));
        }
        i += step;
    }
    Ok((i, out))
}

/// The words of `env -S`'s string; `None` when it is more than words (an operator, a redirection,
/// command substitution): env would not read those the way a shell does.
fn split_string(s: &str) -> Option<Vec<String>> {
    // env's own escapes (`\_` is a space, `\c` ends it) and `${VAR}` are not a shell's
    if s.contains(['\\', '$']) {
        return None;
    }
    let p = parse(s).ok()?;
    if !p.opaque.is_empty() || p.cmds.len() > 1 {
        return None;
    }
    match p.cmds.into_iter().next() {
        None => Some(Vec::new()),
        Some(c) if c.writes.is_empty() && c.stdin.is_none() => Some(c.words),
        Some(_) => None,
    }
}

pub(crate) fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((k, _)) => {
            !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !k.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

pub fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

pub fn argv(words: &[String]) -> Option<Argv> {
    let mut words = words.to_vec();
    let mut sudo = false;
    let mut xargs = false;
    let mut i = 0;
    loop {
        let Some(w) = words.get(i) else {
            // `sudo -i`, `sudo -s`, `sudo`: a root shell
            return sudo.then(|| Argv {
                program: "sudo".into(),
                sudo,
                xargs,
                ..Default::default()
            });
        };
        if KEYWORDS.contains(&w.as_str()) || is_assignment(w) {
            i += 1;
            continue;
        }
        let prog = basename(w).to_string();
        // this word as the program, with what the wrappers before it said
        let (was_sudo, was_xargs, name) = (sudo, xargs, prog.clone());
        let here = move |words: &[String], unclear: Option<String>| Argv {
            program: name,
            args: words[i + 1..].to_vec(),
            sudo: was_sudo,
            xargs: was_xargs,
            unclear,
        };
        let Some(spec) = WRAPPERS.iter().find(|s| s.names.contains(&prog.as_str())) else {
            return Some(here(&words, None));
        };
        let (next, opts) = match options(spec, &words, i + 1) {
            Ok(x) => x,
            Err(why) => return Some(here(&words, Some(why))),
        };
        let has = |names: &[&str]| opts.iter().any(|(o, _)| names.contains(&o.as_str()));
        match prog.as_str() {
            "sudo" | "doas" => sudo = true,
            "xargs" => xargs = true,
            // these look a command up, or act on processes that already run: they run nothing
            "command" if has(&["-v", "-V"]) => return Some(here(&words, None)),
            "taskset" if has(&["-p", "--pid"]) => return Some(here(&words, None)),
            "ionice" if has(&["-p", "-P", "-u", "--pid", "--pgid", "--uid"]) => {
                return Some(here(&words, None));
            }
            _ => {}
        }
        // `env -S 'a b'` runs the words of its string, then the rest
        if let Some((_, v)) = opts
            .iter()
            .find(|(o, _)| o == "-S" || o == "--split-string")
        {
            match v.as_deref().and_then(split_string) {
                Some(split) => {
                    let at = next.min(words.len());
                    words.splice(at..at, split);
                }
                None => {
                    return Some(here(&words, Some("env -S 的字符串看不清是哪些词".into())));
                }
            }
        }
        i = next + spec.lead;
        // `env -`: a lone dash is `-i`, not the command
        if prog == "env" && words.get(i).is_some_and(|w| w == "-") {
            i += 1;
        }
    }
}

/// `-c`, or a cluster of short options that holds it (`-lc`, `-ic`, `-xec`): the next word is the script.
pub fn is_dash_c(w: &str) -> bool {
    w.len() >= 2
        && w.starts_with('-')
        && !w.starts_with("--")
        && w[1..].chars().all(|c| c.is_ascii_alphabetic())
        && w.contains('c')
}

/// Quotes `w` for a POSIX shell when it needs it.
pub fn quote(w: &str) -> String {
    let plain = !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%^".contains(c) || !c.is_ascii());
    if plain {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// The command line of an argv-style command (Codex `exec`: `["/bin/zsh", "-lc", "<script>"]`): the
/// script itself when it is a shell's `-c`, otherwise the words quoted and joined.
pub fn from_argv(words: &[String]) -> String {
    if words.len() == 3 && SHELL_NAMES.contains(&basename(&words[0])) && is_dash_c(&words[1]) {
        return words[2].clone();
    }
    words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

const SHELL_NAMES: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

/// Quoted arguments that are themselves command lines (`sh -c '…'`, `ssh host '…'`, `git rebase -x '…'`,
/// `watch '…'`): every argument with a space or an operator in it, parsed. Arguments that do not
/// parse are skipped, they are not commands.
pub fn nested(args: &[String]) -> Vec<Parsed> {
    args.iter()
        .filter(|a| a.contains([' ', ';', '|', '&', '\n']))
        .filter_map(|a| parse(a).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(src: &str) -> Vec<Vec<String>> {
        parse(src)
            .unwrap()
            .cmds
            .into_iter()
            .map(|c| c.words)
            .collect()
    }

    #[test]
    fn splits_on_operators() {
        assert_eq!(
            words("cargo build && cargo test; ls | wc -l\necho 'a;b' || true"),
            vec![
                vec!["cargo", "build"],
                vec!["cargo", "test"],
                vec!["ls"],
                vec!["wc", "-l"],
                vec!["echo", "a;b"],
                vec!["true"],
            ]
        );
    }

    #[test]
    fn quotes_and_escapes() {
        assert_eq!(
            words(r#"git commit -m "fix \"x\"" a\ b ''"#),
            vec![vec!["git", "commit", "-m", "fix \"x\"", "a b", ""]]
        );
        assert!(parse("echo 'open").is_err());
        assert!(parse("echo \"open").is_err());
    }

    #[test]
    fn redirections() {
        let p = parse("cargo test 2>&1 > out.txt >> /tmp/log &>all 2> err < in.txt").unwrap();
        assert_eq!(p.cmds[0].words, vec!["cargo", "test"]);
        assert_eq!(p.cmds[0].writes, vec!["out.txt", "/tmp/log", "all", "err"]);
        assert!(parse("ls >").is_err());
    }

    #[test]
    fn opaque_constructs() {
        assert_eq!(parse("echo $(whoami)").unwrap().opaque[0], "$(…)");
        assert_eq!(parse("echo `whoami`").unwrap().opaque, vec!["`…`"]);
        assert_eq!(parse("diff <(ls a) <(ls b)").unwrap().opaque[0], "<(…)");
        assert_eq!(parse("(cd .. && rm x)").unwrap().opaque, vec!["(…)"]);
        let p = parse("cat <<EOF\nhi\nEOF").unwrap();
        assert!(p.opaque.is_empty(), "{p:?}");
        assert_eq!(p.cmds[0].stdin.as_deref(), Some("hi\n"));
        assert_eq!(parse("cat <<< hi").unwrap().opaque, vec!["<<<"]);
        let p = parse("cat <<EOF | python3\nimport os\nEOF").unwrap();
        assert_eq!(p.cmds.len(), 2, "{p:?}");
        assert!(!p.cmds[0].piped && p.cmds[1].piped, "{p:?}");
        assert!(parse("a |\n b").unwrap().cmds[1].piped);
        assert!(parse("a |& b").unwrap().cmds[1].piped);
        assert!(!parse("a || b").unwrap().cmds[1].piped);
        assert_eq!(parse("sleep 1 &").unwrap().opaque, vec!["&"]);
        assert!(parse("echo $HOME").unwrap().cmds[0].expands);
        assert!(!parse("echo '$HOME'").unwrap().cmds[0].expands);
    }

    #[test]
    fn argv_strips_wrappers() {
        let a = |s: &str| argv(&parse(s).unwrap().cmds[0].words).unwrap();
        assert_eq!(a("FOO=1 /usr/bin/git push").program, "git");
        let x = a("sudo -u root env -i A=b nohup nice -n 5 timeout 5 rm -rf /x");
        assert_eq!((x.program.as_str(), x.sudo), ("rm", true));
        assert_eq!(x.args, vec!["-rf", "/x"]);
        let x = a("xargs -n 1 rm");
        assert!(x.xargs && x.program == "rm");
        assert_eq!(a("then git push").program, "git");
        assert!(argv(&["A=1".to_string()]).is_none());
    }

    /// ST-26: wrappers are read by their documented options, never by guessing where their command
    /// starts; an option not listed leaves it unknown.
    #[test]
    fn wrappers_are_read_by_their_options() {
        let a = |s: &str| argv(&parse(s).unwrap().cmds[0].words).unwrap();
        let pa = |s: &str| {
            let x = a(s);
            (x.program, x.args, x.sudo, x.unclear.is_some())
        };
        let v = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(pa("caffeinate -i pkill -f node").0, "pkill");
        assert_eq!(pa("caffeinate -t 5 pkill -f node").0, "pkill");
        assert_eq!(pa("time -p pkill -f node").0, "pkill");
        assert_eq!(pa("taskset 0x1 pkill node").0, "pkill");
        assert_eq!(
            pa("taskset -p 0x1 4242").0,
            "taskset",
            "acts on a pid, runs nothing"
        );
        assert_eq!(pa("nice -5 make").0, "make");
        assert_eq!(pa("nice -n5 make").0, "make");
        assert_eq!(pa("timeout --preserve-status -s KILL 5 make").0, "make");
        assert_eq!(pa("stdbuf -oL make").0, "make");
        assert_eq!(pa("xargs -I{} -n 1 rm {}").0, "rm");
        assert_eq!(pa("xargs -i{} rm {}").0, "rm");
        assert_eq!(
            pa("command -v pkill").0,
            "command",
            "looks it up, runs nothing"
        );
        assert_eq!(pa("command pkill").0, "pkill");
        // env -S splits its string into the command
        assert_eq!(
            pa("env -S 'git push origin' HEAD"),
            ("git".into(), v(&["push", "origin", "HEAD"]), false, false)
        );
        assert_eq!(pa("env -i -S'bash -s'").0, "bash");
        assert_eq!(pa("env - pkill -f node").0, "pkill");
        assert_eq!(pa("env --split-string='a b'").1, v(&["b"]));
        assert!(pa("env -S 'a; b'").3, "more than words");
        // a root shell without a command is sudo itself
        for s in ["sudo -i", "sudo -s", "sudo", "sudo -u x -i", "doas -s"] {
            assert_eq!(pa(s), ("sudo".into(), vec![], true, false), "{s}");
        }
        // an option a wrapper is not known to take: where its command starts is not known
        for s in [
            "env --frobnicate pkill",
            "nohup -x pkill",
            "timeout --frob 5 pkill",
        ] {
            let x = pa(s);
            assert!(x.3, "{s}: {x:?}");
            assert_ne!(x.0, "pkill", "{s}");
        }
    }

    #[test]
    fn argv_commands_from_codex() {
        let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            from_argv(&w(&["/bin/zsh", "-lc", "git add a && git commit -m 'x y'"])),
            "git add a && git commit -m 'x y'"
        );
        assert_eq!(from_argv(&w(&["bash", "-c", "ls"])), "ls");
        assert_eq!(
            from_argv(&w(&["git", "commit", "-m", "a b"])),
            "git commit -m 'a b'"
        );
        assert_eq!(from_argv(&w(&["echo", "it's"])), "echo 'it'\\''s'");
        // quoting round-trips through the parser
        let p = parse(&from_argv(&w(&["echo", "it's", "a;b", "$HOME"]))).unwrap();
        assert_eq!(p.cmds.len(), 1);
        assert_eq!(p.cmds[0].words, vec!["echo", "it's", "a;b", "$HOME"]);
        assert!(!p.cmds[0].expands);
        assert!(is_dash_c("-c") && is_dash_c("-lc") && !is_dash_c("--c") && !is_dash_c("-l"));
    }

    #[test]
    fn nested_commands() {
        let n = nested(&["-c".into(), "git push origin main".into()]);
        assert_eq!(n[0].cmds[0].words, vec!["git", "push", "origin", "main"]);
    }
}
