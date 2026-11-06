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
                if matches!(cs.get(i + 1), Some('|' | '&')) {
                    i += 1;
                }
                b.end_cmd();
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

/// Shell keywords and wrappers that run the rest of the words as a command.
const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}", "time",
];

/// The command a simple command really runs: leading `VAR=value` assignments, keywords and wrappers
/// (`env`, `sudo`, `nohup`, `nice`, `timeout`, `command`, `exec`, `xargs`…) removed, the program reduced
/// to its file name. `None` when only assignments remain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argv {
    pub program: String,
    pub args: Vec<String>,
    /// Ran through `sudo`/`doas`.
    pub sudo: bool,
    /// Ran through `xargs`: more arguments come from standard input.
    pub xargs: bool,
}

fn is_assignment(w: &str) -> bool {
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
    let mut sudo = false;
    let mut xargs = false;
    let mut i = 0;
    loop {
        let w = words.get(i)?;
        if KEYWORDS.contains(&w.as_str()) || is_assignment(w) {
            i += 1;
            continue;
        }
        let prog = basename(w);
        // wrappers and the options they take before the command
        let skip_opts = |i: &mut usize, with_value: &[&str]| {
            *i += 1;
            while let Some(o) = words.get(*i) {
                if o == "--" {
                    *i += 1;
                    break;
                }
                if !o.starts_with('-') || o == "-" {
                    break;
                }
                *i += if with_value.contains(&o.as_str()) {
                    2
                } else {
                    1
                };
            }
        };
        match prog {
            "env" => {
                skip_opts(&mut i, &["-u", "-C", "-S", "--unset", "--chdir"]);
                continue;
            }
            "sudo" | "doas" => {
                sudo = true;
                skip_opts(
                    &mut i,
                    &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U"],
                );
                continue;
            }
            "nohup" | "command" | "builtin" | "exec" | "caffeinate" | "stdbuf" | "unbuffer"
            | "chronic" | "ionice" | "taskset" => {
                skip_opts(&mut i, &["-n", "-c", "-w", "-i", "-o", "-e"]);
                continue;
            }
            "nice" => {
                skip_opts(&mut i, &["-n"]);
                continue;
            }
            "timeout" | "gtimeout" => {
                skip_opts(&mut i, &["-s", "-k", "--signal", "--kill-after"]);
                // the duration
                i += 1;
                continue;
            }
            "xargs" => {
                xargs = true;
                skip_opts(
                    &mut i,
                    &[
                        "-I", "-L", "-n", "-P", "-s", "-d", "-E", "-a", "-J", "-R", "-S",
                    ],
                );
                continue;
            }
            _ => {}
        }
        return Some(Argv {
            program: prog.to_string(),
            args: words[i + 1..].to_vec(),
            sudo,
            xargs,
        });
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
