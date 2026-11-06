//! The Node boundary guard (N0e-E15): in production code, nothing outside the Node starts a
//! harness (the Claude/Codex/ACP adapters, `claude -p`, the research harness) or reads project
//! directories, except at the places listed here, each with its reason.
//!
//! It reads the sources of every workspace crate except the Node's own (`steve-node`,
//! `steve-node-cli`), following each crate's module tree from `src/lib.rs` and `src/main.rs` and
//! leaving out comments, literals and test-only items. Three things are counted per file:
//!
//! - [`Rule::Spawn`]: a process started (`Command::new`, `Command::from`, a renamed `Command`,
//!   `fork`/`exec`/`posix_spawn`).
//! - [`Rule::Harness`]: an entry point that starts a harness: the legacy ACP runner, the ACP
//!   adapter connection, the in-process print gateway, in-process research.
//! - [`Rule::Dir`]: a directory listing or a folder walk (`read_dir`, `scan::walk`).
//!
//! Every count must equal its entry in [`ALLOWED`]. A new call site fails the test: route it
//! through the Node (a `WorkspaceExecutor`, `Discovery` or job face), or, if it is truly Hub-host
//! work (the Hub's own data, the upgrader, the keychain, `gh` API calls), add it to the table with
//! why. A removed call site fails too, so the table shrinks with the code: the entries marked
//! [`Until::B7`] go with the legacy runner, those marked [`Until::N0d`] when think and research
//! jobs are the only path, and [`Until::Later`] names the follow-up that moves the rest.

#[path = "node_boundary/scan.rs"]
mod scan;

use std::collections::BTreeMap;
use std::path::Path;

use scan::{Site, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rule {
    Spawn,
    Harness,
    Dir,
}

impl Rule {
    fn patterns(self) -> &'static [&'static str] {
        match self {
            Rule::Spawn => &[
                "Command::new(",
                "Command::from(",
                "process::Command as",
                "libc::fork",
                "libc::exec",
                "posix_spawn",
            ],
            Rule::Harness => &[
                "Acp::new(",
                "steve_acp::rpc",
                "spawn_registered(",
                "spawn_bounded(",
                "LocalPrint {",
                "steve_print::run",
                "Researcher::new(",
                "run_attempt(",
            ],
            Rule::Dir => &["read_dir(", "scan::walk("],
        }
    }
}

/// When an entry goes away. The reasons are for people; the test only checks the counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Until {
    /// Never: Hub-host work, not a project's or a harness's.
    Hub,
    /// The legacy runner and the Hub-side paths of `STEVE_NODE_MODE=off` (B7 deletes them).
    B7,
    /// In-process think and research: `STEVE_NODE_JOBS=off`, and research whose directories the
    /// Node does not hold (N0d).
    N0d,
    /// A named follow-up moves it onto the Node.
    Later(&'static str),
    /// The harness libraries themselves; the Node calls them, and [`Rule::Harness`] bounds who
    /// else may.
    Library,
}

#[allow(dead_code)]
struct Entry {
    rule: Rule,
    /// Under `hub-rs/crates/`.
    file: &'static str,
    count: usize,
    until: Until,
    why: &'static str,
}

const fn e(rule: Rule, file: &'static str, count: usize, until: Until, why: &'static str) -> Entry {
    Entry {
        rule,
        file,
        count,
        until,
        why,
    }
}

use Rule::{Dir, Harness, Spawn};
use Until::{B7, Hub, Later, Library, N0d};

/// Every production call site outside the Node, per rule and file.
#[rustfmt::skip]
const ALLOWED: &[Entry] = &[
    /* the harness libraries: what they start is reached only through the Harness entries */
    e(Spawn, "steve-acp/src/rpc.rs", 1, Library, "an ACP adapter process"),
    e(Spawn, "steve-print/src/lib.rs", 1, Library, "`claude -p` / `codex exec`"),
    e(Spawn, "steve-research/src/run.rs", 1, Library, "one research harness attempt"),
    e(Spawn, "steve-acp/src/config.rs", 1, Hub, "`npm root -g`, to find the adapters"),
    e(Dir, "steve-knowledge/src/scan.rs", 1, Library, "the folder walk the Node's scan reuses"),
    /* harness entry points outside the Node */
    e(Harness, "steve-hub/src/wire/mod.rs", 1, B7, "Acp::new: the legacy ACP runner"),
    e(Harness, "steve-hub/src/wire/mod.rs", 1, N0d, "LocalPrint: the in-process think gateway"),
    e(Harness, "steve-core/src/gateway.rs", 3, N0d, "LocalPrint and its steve_print::run"),
    e(Harness, "steve-hub/src/wire/research.rs", 1, N0d, "in-process research runs"),
    e(Spawn, "steve-hub/src/catalog.rs", 1, Later("the model catalogue from the Node"),
        "`codex debug models`, read-only in CODEX_HOME"),
    /* project directories on the Hub host */
    e(Spawn, "steve-core/src/git.rs", 1, B7, "git for items with no Node (STEVE_NODE_MODE=off)"),
    e(Spawn, "steve-github/src/push.rs", 1, B7, "the Hub-side push when no Node holds the item"),
    e(Spawn, "steve-modules/src/vault/inject.rs", 1, B7, "a secret use with no Node"),
    e(Dir, "steve-hub/src/api/outputs.rs", 1, B7, "the outputs listing with no Node"),
    e(Dir, "steve-knowledge/src/sources/folder.rs", 1, B7, "the knowledge folder walk with no Node"),
    e(Dir, "steve-acp/src/rollout.rs", 1, B7, "Codex rollout files of the legacy runner"),
    e(Spawn, "steve-github/src/repos.rs", 1, Later("a project's remote from Node git facts"),
        "`git remote get-url` of a project"),
    e(Spawn, "steve-core/src/lane/exec.rs", 2, Later("E12 lanes on the Node"),
        "a lane's checks and the repository's release step"),
    e(Spawn, "steve-core/src/workspace.rs", 1, Later("E14 disk housekeeping (D13)"),
        "`lsof -d cwd` before caches are shed"),
    e(Dir, "steve-core/src/workspace.rs", 6, Later("E14 disk housekeeping (D13)"),
        "worktree cache sizes and the trash"),
    e(Dir, "steve-hub/src/api/projects.rs", 2, Later("projects describe and browse on the Node"),
        "the describe input, and browsing with no Node"),
    e(Spawn, "steve-core/src/release.rs", 2, Hub, "D12: the release script stays Hub-host for device=hub"),
    /* Hub-host work */
    e(Spawn, "steve-core/src/lane/profile.rs", 1, Hub, "`gh api`"),
    e(Spawn, "steve-core/src/takeover.rs", 1, Hub, "`gh run list`"),
    e(Spawn, "steve-core/src/yolo/procs.rs", 1, Hub, "`ps`: the process table"),
    e(Spawn, "steve-hub/src/config.rs", 1, Hub, "npm config"),
    e(Spawn, "steve-hub/src/system.rs", 1, Hub, "host facts (sw_vers, sysctl, uname, hostname)"),
    e(Spawn, "steve-hub/src/upgrades/runner.rs", 6, Hub, "the upgrader"),
    e(Spawn, "steve-hub/src/upgrades/supervisor.rs", 1, Hub, "the supervisor starts the Hub"),
    e(Spawn, "steve-keyhelper/src/mac.rs", 2, Hub, "the keychain helper"),
    e(Spawn, "steve-modules/src/vault/keystore/helper.rs", 1, Hub, "the vault keystore helper"),
    e(Spawn, "steve-modules/src/net.rs", 1, Hub, "test support: reruns one test in a child"),
    e(Dir, "steve-hub/src/upgrades/mod.rs", 1, Hub, "upgrade operations in the Hub data dir"),
    e(Dir, "steve-modules/src/vault/inject.rs", 1, Hub, "the vault's scratch dirs in the Hub data dir"),
    e(Dir, "steve-knowledge/src/eval.rs", 1, Hub, "the offline retrieval eval corpus"),
];

/// The crates that are the Node.
const NODE: &[&str] = &["steve-node", "steve-node-cli"];
/// The crates that hold harness code; [`Rule::Harness`] is about their callers.
const LIBRARIES: &[&str] = &["steve-acp", "steve-print", "steve-research"];

fn crates_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

/// Every production source outside the Node, by crate.
fn sources() -> Vec<(String, Vec<Source>)> {
    let mut names: Vec<String> = std::fs::read_dir(crates_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !NODE.contains(&n.as_str()))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| {
            let s = scan::crate_sources(crates_dir(), &n);
            (n, s)
        })
        .collect()
}

/// The sites of `rule` in one crate's sources.
fn sites(rule: Rule, krate: &str, sources: &[Source]) -> Vec<Site> {
    if rule == Rule::Harness && LIBRARIES.contains(&krate) {
        return vec![];
    }
    let mut out: Vec<Site> = rule
        .patterns()
        .iter()
        .flat_map(|p| scan::find(sources, p))
        .collect();
    out.sort();
    out
}

/// What differs between the call sites found and `allowed`, one line each.
fn check(found: &BTreeMap<(Rule, String), Vec<Site>>, allowed: &[Entry]) -> Vec<String> {
    let mut want: BTreeMap<(Rule, String), usize> = BTreeMap::new();
    for a in allowed {
        *want.entry((a.rule, a.file.to_string())).or_default() += a.count;
    }
    let mut out = Vec::new();
    for ((rule, file), sites) in found {
        let lines: Vec<String> = sites.iter().map(|s| s.line.to_string()).collect();
        let w = want.get(&(*rule, file.clone())).copied().unwrap_or(0);
        if sites.len() > w {
            out.push(format!(
                "{rule:?} {file}:{} — {} site(s), {w} allowed: go through the Node, or list it \
                 in ALLOWED with why",
                lines.join(","),
                sites.len()
            ));
        } else if sites.len() < w {
            out.push(format!(
                "{rule:?} {file}:{} — {} site(s), ALLOWED still says {w}: lower or drop the entry",
                lines.join(","),
                sites.len()
            ));
        }
    }
    for ((rule, file), w) in &want {
        if !found.contains_key(&(*rule, file.clone())) {
            out.push(format!(
                "{rule:?} {file} — no site left, ALLOWED still says {w}: drop the entry"
            ));
        }
    }
    out
}

fn found_in(krate: &str, src: &[Source], found: &mut BTreeMap<(Rule, String), Vec<Site>>) {
    for rule in [Rule::Spawn, Rule::Harness, Rule::Dir] {
        for s in sites(rule, krate, src) {
            found.entry((rule, s.file.clone())).or_default().push(s);
        }
    }
}

/// The guard itself: production code outside the Node starts harnesses and processes, and lists
/// directories, only where [`ALLOWED`] says.
#[test]
fn nothing_outside_the_node_starts_a_harness_or_walks_a_project_unlisted() {
    let mut found = BTreeMap::new();
    for (krate, src) in sources() {
        found_in(&krate, &src, &mut found);
    }
    let problems = check(&found, ALLOWED);
    assert!(
        problems.is_empty(),
        "the Node boundary (N0e-E15):\n{}",
        problems.join("\n")
    );
}

/// Each entry says why and names a file that is there, outside the Node.
#[test]
fn every_allowed_entry_has_a_reason_and_a_file() {
    for a in ALLOWED {
        assert!(!a.why.trim().is_empty(), "{}: no reason", a.file);
        assert!(a.count > 0, "{}: a zero entry", a.file);
        assert!(
            crates_dir().join(a.file).is_file(),
            "{}: no such file",
            a.file
        );
        assert!(
            !NODE.iter().any(|n| a.file.starts_with(&format!("{n}/"))),
            "{}: the Node needs no entry",
            a.file
        );
    }
}

/// The scan reaches the Hub's real wiring (so finding nothing is never a pass by accident), and
/// leaves out the Node and test-only files.
#[test]
fn the_scan_sees_the_hub_and_not_the_node() {
    let all = sources();
    let files: Vec<&str> = all
        .iter()
        .flat_map(|(_, s)| s.iter().map(|s| s.rel.as_str()))
        .collect();
    for f in [
        "steve-hub/src/wire/mod.rs",
        "steve-hub/src/wire/node_jobs.rs", // through `#[path]`
        "steve-core/src/gateway.rs",
        "steve-acp/src/rpc.rs",
        "steve-keyhelper/src/mac.rs",
    ] {
        assert!(files.contains(&f), "{f} not scanned");
    }
    for f in &files {
        assert!(!NODE.iter().any(|n| f.starts_with(&format!("{n}/"))), "{f}");
    }
    // `#[cfg(test)] mod tests;` and the `fake` feature's modules are left out
    for f in [
        "steve-hub/src/upgrades/tests.rs",
        "steve-core/src/yolo/engine_tests.rs",
        "steve-github/src/fake/git.rs",
    ] {
        assert!(!files.contains(&f), "{f} scanned");
    }
}

/// A new harness spawn or project walk in Hub code fails the guard; the same text in comments,
/// literals or test-only items does not.
#[test]
fn a_new_call_site_fails_and_comments_literals_and_tests_do_not() {
    let src = r##"
//! `Command::new("claude")` in a doc comment
/* Command::new("x") /* nested */ read_dir( */
fn label<'a>(s: &'a str) -> char { let _ = "Command::new(\"q\") read_dir("; let _ = r#"Acp::new("#; '{' }
fn production() {
    let _ = tokio::process::Command::new(&cfg.claude_executable);
    let _ = steve_acp::Acp::new(cfg, host);
    let _ = std::fs::read_dir(project);
}
#[cfg(test)]
mod tests {
    fn t() { let _ = std::process::Command::new("sleep"); let _ = "}"; }
}
#[cfg(all(test, unix))]
fn only_in_tests() { std::process::Command::new("ps"); }
#[cfg(feature = "fake")]
pub mod fake { fn f() { std::process::Command::new("git"); } }
#[cfg(not(test))]
fn not_test() { let _ = std::fs::read_dir("/"); }
"##;
    let s = vec![Source {
        rel: "steve-x/src/lib.rs".into(),
        code: scan::production(src).unwrap(),
    }];
    let lines =
        |rule| -> Vec<usize> { sites(rule, "steve-x", &s).iter().map(|s| s.line).collect() };
    assert_eq!(lines(Rule::Spawn), vec![6]);
    assert_eq!(lines(Rule::Harness), vec![7]);
    assert_eq!(lines(Rule::Dir), vec![8, 19]);
    // a harness library's own entry points are not its callers
    assert!(sites(Rule::Harness, "steve-acp", &s).is_empty());

    let mut found = BTreeMap::new();
    found_in("steve-x", &s, &mut found);
    // unlisted: all three fail
    let p = check(&found, &[]);
    assert_eq!(p.len(), 3, "{p:?}");
    assert!(p[0].starts_with("Spawn steve-x/src/lib.rs:6 "), "{p:?}");
    // listed: passes
    let listed = [
        e(Spawn, "steve-x/src/lib.rs", 1, Hub, "t"),
        e(Harness, "steve-x/src/lib.rs", 1, B7, "t"),
        e(Dir, "steve-x/src/lib.rs", 2, Hub, "t"),
    ];
    assert!(check(&found, &listed).is_empty());
    // one site more than listed fails, and so does an entry with no site left
    let short = [
        e(Spawn, "steve-x/src/lib.rs", 1, Hub, "t"),
        e(Harness, "steve-x/src/lib.rs", 1, B7, "t"),
        e(Dir, "steve-x/src/lib.rs", 1, Hub, "t"),
        e(Spawn, "steve-y/src/gone.rs", 1, B7, "t"),
    ];
    let p = check(&found, &short);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(
        p.iter()
            .any(|l| l.starts_with("Dir steve-x/src/lib.rs:8,19 "))
    );
    assert!(
        p.iter()
            .any(|l| l.starts_with("Spawn steve-y/src/gone.rs — no site left"))
    );
}

/// A test-only file (`#![cfg(test)]`) is left out whole; another inner `cfg` is not.
#[test]
fn a_test_only_file_is_left_out() {
    assert!(scan::production("//! doc\n#![cfg(test)]\nfn f() { Command::new(\"x\"); }").is_none());
    assert!(scan::production("#![cfg(unix)]\nfn f() {}").is_some());
}
