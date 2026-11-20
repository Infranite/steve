//! SG15 representative repository command corpus and grammar boundaries. No command is executed.
use super::{judge_tests::Snapshot, shell};
use serde_json::{Value, json};

fn parsed(command: &str) -> Value {
    match shell::parse(command) {
        Err(e) => json!({"error":e.0}),
        Ok(p) => json!({"commands":p.cmds.iter().map(|c| {
            let argv=shell::argv(&c.words).map(|a|json!({"program":a.program,"args":a.args,"sudo":a.sudo,"xargs":a.xargs,"unclear":a.unclear}));
            json!({"words":c.words,"writes":c.writes,"expands":c.expands,"stdin":c.stdin,"piped":c.piped,"argv":argv})
        }).collect::<Vec<_>>(),"opaque":p.opaque}),
    }
}

const COMMANDS: &[(&str, &str)] = &[
    (
        "existing:src/yolo/shell.rs",
        "cargo build && cargo test; ls | wc -l\necho 'a;b' || true",
    ),
    ("existing:src/yolo/shell.rs", "cargo"),
    ("existing:src/yolo/shell.rs", "ls"),
    ("existing:src/yolo/shell.rs", "echo"),
    ("existing:src/yolo/shell.rs", "a;b"),
    (
        "existing:src/yolo/shell.rs",
        "git commit -m \"fix \\\"x\\\"\" a\\ b ''",
    ),
    ("existing:src/yolo/shell.rs", "git"),
    ("existing:src/yolo/shell.rs", "echo 'open"),
    ("existing:src/yolo/shell.rs", "echo \"open"),
    (
        "existing:src/yolo/shell.rs",
        "cargo test 2>&1 > out.txt >> /tmp/log &>all 2> err < in.txt",
    ),
    ("existing:src/yolo/shell.rs", "ls >"),
    ("existing:src/yolo/shell.rs", "echo $(whoami)"),
    ("existing:src/yolo/shell.rs", "echo `whoami`"),
    ("existing:src/yolo/shell.rs", "diff <(ls a) <(ls b)"),
    ("existing:src/yolo/shell.rs", "(cd .. && rm x)"),
    ("existing:src/yolo/shell.rs", "cat <<EOF\nhi\nEOF"),
    ("existing:src/yolo/shell.rs", "cat <<< hi"),
    (
        "existing:src/yolo/shell.rs",
        "cat <<EOF | python3\nimport os\nEOF",
    ),
    ("existing:src/yolo/shell.rs", "sleep 1 &"),
    ("existing:src/yolo/shell.rs", "echo $HOME"),
    ("existing:src/yolo/shell.rs", "echo '$HOME'"),
    ("existing:src/yolo/shell.rs", "FOO=1 /usr/bin/git push"),
    (
        "existing:src/yolo/shell.rs",
        "sudo -u root env -i A=b nohup nice -n 5 timeout 5 rm -rf /x",
    ),
    ("existing:src/yolo/shell.rs", "rm"),
    ("existing:src/yolo/shell.rs", "xargs -n 1 rm"),
    ("existing:src/yolo/shell.rs", "then git push"),
    ("existing:src/yolo/shell.rs", "caffeinate -i pkill -f node"),
    (
        "existing:src/yolo/shell.rs",
        "caffeinate -t 5 pkill -f node",
    ),
    ("existing:src/yolo/shell.rs", "time -p pkill -f node"),
    ("existing:src/yolo/shell.rs", "taskset 0x1 pkill node"),
    ("existing:src/yolo/shell.rs", "taskset -p 0x1 4242"),
    ("existing:src/yolo/shell.rs", "taskset"),
    ("existing:src/yolo/shell.rs", "nice -5 make"),
    ("existing:src/yolo/shell.rs", "nice -n5 make"),
    (
        "existing:src/yolo/shell.rs",
        "timeout --preserve-status -s KILL 5 make",
    ),
    ("existing:src/yolo/shell.rs", "stdbuf -oL make"),
    ("existing:src/yolo/shell.rs", "xargs -I{} -n 1 rm {}"),
    ("existing:src/yolo/shell.rs", "xargs -i{} rm {}"),
    ("existing:src/yolo/shell.rs", "command -v pkill"),
    ("existing:src/yolo/shell.rs", "command"),
    ("existing:src/yolo/shell.rs", "command pkill"),
    (
        "existing:src/yolo/shell.rs",
        "env -S 'git push origin' HEAD",
    ),
    ("existing:src/yolo/shell.rs", "env -i -S'bash -s'"),
    ("existing:src/yolo/shell.rs", "bash"),
    ("existing:src/yolo/shell.rs", "env - pkill -f node"),
    ("existing:src/yolo/shell.rs", "env --split-string='a b'"),
    ("existing:src/yolo/shell.rs", "env -S 'a; b'"),
    ("existing:src/yolo/shell.rs", "sudo -i"),
    ("existing:src/yolo/shell.rs", "sudo -s"),
    ("existing:src/yolo/shell.rs", "sudo"),
    ("existing:src/yolo/shell.rs", "sudo -u x -i"),
    ("existing:src/yolo/shell.rs", "doas -s"),
    ("existing:src/yolo/shell.rs", "env --frobnicate pkill"),
    ("existing:src/yolo/shell.rs", "nohup -x pkill"),
    ("existing:src/yolo/shell.rs", "timeout --frob 5 pkill"),
    (
        "existing:src/yolo/shell.rs",
        "git add a && git commit -m 'x y'",
    ),
    ("existing:src/yolo/shell.rs", "git commit -m 'a b'"),
    ("existing:src/yolo/shell.rs", "echo 'it'\\''s'"),
    ("existing:src/yolo/shell.rs", "git push origin main"),
    ("synthetic:POSIX-boundary", ""),
    ("synthetic:POSIX-boundary", "# only comment"),
    ("synthetic:POSIX-boundary", "\\"),
    ("synthetic:POSIX-boundary", "echo a\\\nb"),
    ("synthetic:POSIX-boundary", "''"),
    ("synthetic:POSIX-boundary", "\"\""),
    ("synthetic:POSIX-boundary", "echo 中文; printf '%s' 'é'"),
    ("synthetic:POSIX-boundary", "echo a#b # comment\necho c"),
    ("synthetic:POSIX-boundary", "echo \\\njoined"),
    (
        "synthetic:POSIX-boundary",
        "cat <<-EOF\n\tbody\n\tEOF\necho done",
    ),
    (
        "synthetic:POSIX-boundary",
        "cat <<A <<'B'\n$(id)\nA\n`id`\nB",
    ),
    (
        "synthetic:POSIX-boundary",
        "cat <<'EOF'\n$HOME `id` $(id)\nEOF",
    ),
    ("synthetic:POSIX-boundary", "cat <<EOF\nbody"),
    ("synthetic:POSIX-boundary", "cat <<"),
    ("synthetic:POSIX-boundary", "cat <<''\n"),
    ("synthetic:POSIX-boundary", "echo >"),
    ("synthetic:POSIX-boundary", "echo >|file"),
    ("synthetic:POSIX-boundary", "echo &>>file"),
    ("synthetic:POSIX-boundary", "echo >&-"),
    ("synthetic:POSIX-boundary", "echo 10>&2"),
    ("synthetic:POSIX-boundary", "cat <file; echo ok"),
    ("synthetic:POSIX-boundary", "echo 3<file"),
    ("synthetic:POSIX-boundary", "echo >\"a b\""),
    ("synthetic:POSIX-boundary", "echo a\\;b"),
    ("synthetic:POSIX-boundary", "echo $(id) `id` $(id)"),
    ("synthetic:POSIX-boundary", "cat <<< hi >ignored"),
    ("synthetic:POSIX-boundary", "eval 'echo ok'"),
    ("synthetic:POSIX-boundary", "echo 'eval echo ok'"),
    ("synthetic:POSIX-boundary", "echo '$(id)'"),
    ("synthetic:POSIX-boundary", "echo \"$(id)\""),
    ("synthetic:POSIX-boundary", "echo \\$HOME"),
    ("synthetic:POSIX-boundary", "echo \"\\$HOME\""),
    ("synthetic:POSIX-boundary", "echo \"\\q\""),
    ("synthetic:POSIX-boundary", "echo \"`id`\""),
    ("synthetic:POSIX-boundary", "echo \\`id\\`"),
    ("synthetic:POSIX-boundary", "diff <(ls a) b"),
    ("synthetic:POSIX-boundary", "cat >(ls a)"),
    ("synthetic:POSIX-boundary", "echo '>(ls a)'"),
    ("synthetic:POSIX-boundary", "(echo ok)"),
    ("synthetic:POSIX-boundary", "echo '(echo ok)'"),
    ("synthetic:POSIX-boundary", "echo '&'"),
    ("synthetic:POSIX-boundary", "echo '<<< hi'"),
    ("synthetic:POSIX-boundary", "echo `id`"),
    ("synthetic:POSIX-boundary", "echo >a 2>&1 |& cat"),
    (
        "synthetic:POSIX-boundary",
        "echo ok &&\n # middle\n echo next",
    ),
];

#[test]
fn sg15_representative_shell_golden() {
    let mut g = Snapshot::new("shell.json");
    for (i, (source, command)) in COMMANDS.iter().enumerate() {
        g.row(
            format!("shell/{i}"),
            json!({"source":source,"command":command,"parsed":parsed(command)}),
        );
    }
    g.finish();
}

#[test]
fn sg15_unreadable_constructs_have_positive_and_quoted_or_escaped_counterparts() {
    for (danger, literal, flag) in [
        ("echo $(whoami)", "echo '$whoami'", "$(…)"),
        ("echo \"$(whoami)\"", "echo '\u{24}(whoami)'", "$(…)"),
        ("echo `whoami`", "echo '`whoami`'", "`…`"),
        ("echo \"`whoami`\"", "echo \\`whoami\\`", "`…`"),
        ("diff <(ls a) b", "echo '<(ls a)'", "<(…)"),
        ("cat >(ls a)", "echo '>(ls a)'", "<(…)"),
        ("(echo ok)", "echo '(echo ok)'", "(…)"),
        ("cat <<< hi", "echo '<<< hi'", "<<<"),
        ("sleep 1 &", "echo '&'", "&"),
        (
            "cat <<EOF\n$(whoami)\nEOF",
            "cat <<'EOF'\n$(whoami)\nEOF",
            "$(…)",
        ),
        (
            "cat <<EOF\n`whoami`\nEOF",
            "cat <<'EOF'\n`whoami`\nEOF",
            "`…`",
        ),
    ] {
        assert!(
            shell::parse(danger).unwrap().opaque.contains(&flag),
            "{danger}"
        );
        assert!(
            shell::parse(literal).unwrap().opaque.is_empty(),
            "{literal}"
        );
    }
    // eval is visible as the executable word, not a Parsed::opaque marker. The
    // guard rejects that program; changing this parser must never erase the word.
    let p = shell::parse("eval 'echo ok'").unwrap();
    assert_eq!(shell::argv(&p.cmds[0].words).unwrap().program, "eval");
    let p = shell::parse("echo 'eval echo ok'").unwrap();
    assert_eq!(shell::argv(&p.cmds[0].words).unwrap().program, "echo");
    for (unreadable, literal) in [
        ("echo $HOME", "echo '$HOME'"),
        ("echo \"$HOME\"", "echo \\$HOME"),
    ] {
        assert!(
            shell::parse(unreadable).unwrap().cmds[0].expands,
            "{unreadable}"
        );
        assert!(!shell::parse(literal).unwrap().cmds[0].expands, "{literal}");
    }
    for (command, error) in [
        ("echo \\", "命令以反斜杠结尾"),
        ("echo '", "引号没有闭合"),
        ("echo \"", "引号没有闭合"),
        ("cat <<", "here-document 缺少结束标记"),
        ("cat <<EOF\nbody", "here-document 没有结束"),
        ("echo >", "重定向缺少目标"),
    ] {
        assert_eq!(shell::parse(command).unwrap_err().0, error, "{command}");
    }
    let p = shell::parse("a |\n b 2>&1 >out && c || d").unwrap();
    assert_eq!(
        p.cmds.iter().map(|c| c.piped).collect::<Vec<_>>(),
        [false, true, false, false]
    );
    assert_eq!(p.cmds[1].writes, ["out"]);
}
