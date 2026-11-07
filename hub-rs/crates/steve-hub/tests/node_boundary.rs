//! The Node boundary guard (N0e-E15): in production code, nothing outside the Node starts a
//! harness (the Claude/Codex/ACP adapters, `claude -p`, the research harness), starts a process,
//! or touches files and directories, except at the places listed here, each with its reason.
//!
//! It reads the sources of every workspace crate except the Node's own (`steve-node`,
//! `steve-node-cli`), following each crate's module tree from `src/lib.rs` and `src/main.rs` and
//! leaving out comments, literals and test-only items. Four things are counted per file:
//!
//! - [`Rule::Spawn`]: a process started (`Command::new`, `Command::from`, a renamed `Command`,
//!   `fork`/`exec`/`posix_spawn`).
//! - [`Rule::Harness`]: an entry point that starts a harness, or a handle on one: the legacy ACP
//!   runner, the ACP adapter connection, the in-process print gateway, in-process research.
//! - [`Rule::Dir`]: a directory listing or a folder walk (`read_dir`, `scan::walk`).
//! - [`Rule::Fs`]: a file opened, read, written, copied, renamed or removed, or a directory created
//!   or removed (`std::fs` and `tokio::fs`). Most are the Hub's own data; the rest are project
//!   and worktree paths on the Hub host, each with the follow-up that moves it.
//!
//! A call is found by its text, and an import by its item: `use a::{b::{c as d}}` counts as
//! `a::b::c as d`, so a braced, renamed or glob import of a listed item counts where the call
//! itself would not show (`run_supervised(`, `Cmd::new(`, `remove_dir_all(`). The guard is a
//! text scan, not a type check: it does not see a value that holds a harness passed elsewhere and
//! used there, only where such values are made or their modules imported.
//!
//! Every count must equal its entry in [`ALLOWED`]. A new call site fails the test: route it
//! through the Node (a `WorkspaceExecutor`, `Discovery` or job face), or, if it is truly Hub-host
//! work (the Hub's own data, the upgrader, the keychain, `gh` API calls), add it to the table with
//! why. A removed call site fails too, so the table shrinks with the code. The legacy runner's
//! entries went with it (B7): nothing outside the Node starts a session's harness. Those marked
//! [`Until::NoNode`] serve items no Node holds and go with `STEVE_NODE_MODE=off`, those marked
//! [`Until::N0d`] when think and research jobs are the only path, and [`Until::Later`] names the
//! follow-up that moves the rest.

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
    Fs,
}

/// How an import of an item counts (see `scan::imports`).
#[derive(Clone, Copy)]
enum Import {
    /// Any import of it, of something under it, or a renamed or glob import of a module above it:
    /// its uses do not name it in full (`run_supervised(`, `Conn::spawn(`).
    Any,
    /// Only a rename (`Command as Cmd`): its uses are found by its own name.
    Renamed,
}

use Import::{Any, Renamed};

const RULES: [Rule; 4] = [Rule::Spawn, Rule::Harness, Rule::Dir, Rule::Fs];

impl Rule {
    /// What is counted in the code, as text.
    fn patterns(self) -> &'static [&'static str] {
        match self {
            Rule::Spawn => &[
                "Command::new(",
                "Command::from(",
                "libc::fork",
                "libc::exec",
                "posix_spawn",
            ],
            Rule::Harness => &[
                "Acp::new(",
                // `Conn::spawn(` is not a pattern: the Node link's own `Conn` has one too; an
                // import of `steve_acp::rpc` (or anything in it) is counted instead
                "steve_acp::rpc",
                "spawn_registered(",
                "spawn_bounded(",
                "LocalPrint {",
                "steve_print::run",
                "PrintRun {",
                "Researcher::new(",
                "researcher.start(",
                "run_attempt(",
            ],
            Rule::Dir => &["read_dir(", "scan::walk("],
            Rule::Fs => &[
                "File::open(",
                "File::create(",
                "File::create_new(",
                "File::options(",
                "OpenOptions::new(",
                "fs::read(",
                "fs::read_to_string(",
                "fs::write(",
                "fs::copy(",
                "fs::rename(",
                "fs::hard_link(",
                "fs::symlink(",
                "fs::set_permissions(",
                "fs::create_dir(",
                "fs::create_dir_all(",
                "fs::remove_file(",
                "fs::remove_dir(",
                "fs::remove_dir_all(",
            ],
        }
    }

    /// What is counted where it is imported: the imports that would hide a use from
    /// [`Rule::patterns`].
    fn imports(self) -> &'static [(&'static str, Import)] {
        match self {
            Rule::Spawn => &[
                ("std::process::Command", Renamed),
                ("tokio::process::Command", Renamed),
                ("libc::fork", Any),
                ("libc::execv", Any),
                ("libc::execvp", Any),
                ("libc::execve", Any),
                ("libc::posix_spawn", Any),
                ("libc::posix_spawnp", Any),
            ],
            Rule::Harness => &[
                ("steve_acp::Acp", Renamed),
                ("steve_acp::rpc", Any),
                ("steve_print::run", Any),
                ("steve_print::run_supervised", Any),
                ("steve_print::PrintRun", Any),
                ("steve_core::LocalPrint", Renamed),
                ("steve_core::gateway::LocalPrint", Renamed),
                ("steve_research::Researcher", Any),
                ("steve_research::run_attempt", Any),
                ("steve_research::drive", Any),
            ],
            Rule::Dir => &[
                ("std::fs::read_dir", Renamed),
                ("tokio::fs::read_dir", Renamed),
            ],
            Rule::Fs => &FS_IMPORTS,
        }
    }
}

/// The `std::fs` and `tokio::fs` items of [`Rule::Fs`]: a type is used by its name
/// (`File::open(`), a function bare once imported (`remove_dir_all(`).
const FS_IMPORTS: [(&str, Import); 30] = [
    ("std::fs::File", Renamed),
    ("std::fs::OpenOptions", Renamed),
    ("std::fs::read", Any),
    ("std::fs::read_to_string", Any),
    ("std::fs::write", Any),
    ("std::fs::copy", Any),
    ("std::fs::rename", Any),
    ("std::fs::hard_link", Any),
    ("std::fs::set_permissions", Any),
    ("std::fs::create_dir", Any),
    ("std::fs::create_dir_all", Any),
    ("std::fs::remove_file", Any),
    ("std::fs::remove_dir", Any),
    ("std::fs::remove_dir_all", Any),
    ("std::os::unix::fs::symlink", Any),
    ("tokio::fs::File", Renamed),
    ("tokio::fs::OpenOptions", Renamed),
    ("tokio::fs::read", Any),
    ("tokio::fs::read_to_string", Any),
    ("tokio::fs::write", Any),
    ("tokio::fs::copy", Any),
    ("tokio::fs::rename", Any),
    ("tokio::fs::hard_link", Any),
    ("tokio::fs::set_permissions", Any),
    ("tokio::fs::create_dir", Any),
    ("tokio::fs::create_dir_all", Any),
    ("tokio::fs::remove_file", Any),
    ("tokio::fs::remove_dir", Any),
    ("tokio::fs::remove_dir_all", Any),
    ("tokio::fs::symlink", Any),
];

/// When an entry goes away. The reasons are for people; the test only checks the counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Until {
    /// Never: Hub-host work, not a project's or a harness's.
    Hub,
    /// Items no Node holds, read on the Hub host: with `STEVE_NODE_MODE=off` (the rollback), or
    /// an item whose sessions all ended on the legacy runner before B7 removed it. They go with
    /// the off mode. No session runs without a Node any more.
    NoNode,
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

use Rule::{Dir, Fs, Harness, Spawn};
use Until::{Hub, Later, Library, N0d, NoNode};

/// Every production call site outside the Node, per rule and file.
#[rustfmt::skip]
const ALLOWED: &[Entry] = &[
    /* the harness libraries: what they start is reached only through the Harness entries */
    e(Spawn, "steve-acp/src/rpc.rs", 1, Library, "an ACP adapter process"),
    e(Spawn, "steve-print/src/lib.rs", 1, Library, "`claude -p` / `codex exec`"),
    e(Spawn, "steve-research/src/run.rs", 1, Library, "one research harness attempt"),
    e(Spawn, "steve-acp/src/config.rs", 1, Hub, "`npm root -g`, to find the adapters"),
    e(Spawn, "steve-vault/src/harness.rs", 1, Library, "whether seatbelt starts here (sandbox-exec on /usr/bin/true): the Node's sandbox and the pre-switch check"),
    e(Dir, "steve-vault/src/harness.rs", 1, Library, "the protected name prefixes the Node hides from a bubblewrap run"),
    e(Dir, "steve-toolchains/src/lib.rs", 2, Library, "a project's marker files, for the Node to set its toolchains' places"),
    e(Dir, "steve-knowledge/src/scan.rs", 1, Library, "the folder walk the Node's scan reuses"),
    e(Fs, "steve-acp/src/codex.rs", 4, Library, "CODEX_HOME config and the adapter's instructions file"),
    e(Dir, "steve-acp/src/rollout.rs", 1, Library, "Codex rollout files: the Node's history, the stats repair"),
    e(Fs, "steve-acp/src/rollout.rs", 1, Library, "Codex rollout files: the Node's history, the stats repair"),
    e(Fs, "steve-print/src/codex.rs", 6, Library, "`codex exec` image and instructions files"),
    e(Fs, "steve-research/src/attach.rs", 4, Library, "attachments in a research run's dir"),
    e(Fs, "steve-research/src/run.rs", 8, Library, "a research run's dir, sandbox profile and MCP config"),
    /* harness entry points outside the Node */
    e(Harness, "steve-hub/src/wire/mod.rs", 1, N0d, "LocalPrint: the in-process think gateway"),
    e(Harness, "steve-core/src/gateway.rs", 4, N0d, "LocalPrint, its PrintRun and steve_print::run"),
    e(Harness, "steve-hub/src/wire/research.rs", 3, N0d,
        "in-process research runs: the steve_research import, Researcher::new and its start"),
    e(Spawn, "steve-hub/src/catalog.rs", 1, Later("the model catalogue from the Node"),
        "`codex debug models`, read-only in CODEX_HOME"),
    /* project directories on the Hub host */
    e(Spawn, "steve-core/src/git.rs", 1, NoNode, "git for items no Node holds"),
    e(Spawn, "steve-github/src/push.rs", 1, NoNode, "the Hub-side push when no Node holds the item"),
    e(Spawn, "steve-modules/src/vault/inject.rs", 1, NoNode, "a secret use with no Node"),
    e(Dir, "steve-hub/src/api/outputs.rs", 1, NoNode, "the outputs listing with no Node"),
    e(Dir, "steve-knowledge/src/sources/folder.rs", 1, NoNode, "the knowledge folder walk with no Node"),
    e(Fs, "steve-hub/src/api/outputs.rs", 2, NoNode, "an output read from disk when no Node holds the item"),
    e(Fs, "steve-knowledge/src/sources/folder.rs", 1, NoNode, "a knowledge folder file read with no Node"),
    e(Fs, "steve-core/src/cycle.rs", 3, NoNode, "the worktree fingerprint for items no Node holds (its throwaway index and the lock a killed git leaves)"),
    e(Fs, "steve-github/src/push.rs", 1, NoNode, "the Hub-side push borrows the project's git objects"),
    e(Fs, "steve-core/src/lane/exec.rs", 2, Later("E12 lanes on the Node"),
        "a lane's worktree removed, and its cut-release.py read for --ci-wait support"),
    e(Fs, "steve-core/src/lane/drive.rs", 1, Later("E12 lanes on the Node"),
        "an ended unit's worktree removed"),
    e(Fs, "steve-core/src/workspace.rs", 5, Later("E14 disk housekeeping (D13)"),
        "workspace folders created and moved to the trash"),
    e(Fs, "steve-hub/src/api/projects.rs", 1, Later("projects describe and browse on the Node"),
        "the README read for the describe input"),
    e(Fs, "steve-core/src/orchestrator.rs", 2, Later("the queue's cwd check from the Node"),
        "the write probe in a session's cwd"),
    e(Fs, "steve-knowledge/src/sources/items.rs", 1, Later("item outputs read through the Node"),
        "the text of an item's outputs in its cwd"),
    e(Fs, "steve-hub/src/catalog.rs", 1, Later("the model catalogue from the Node"),
        "the Claude Code model picker settings"),
    e(Fs, "steve-core/src/models.rs", 1, Later("the model catalogue from the Node"),
        "the default model in CODEX_HOME/config.toml"),
    e(Spawn, "steve-github/src/repos.rs", 1, Later("a project's remote from Node git facts"),
        "`git remote get-url` of a project"),
    e(Spawn, "steve-core/src/lane/exec.rs", 1, Later("E12 lanes on the Node"),
        "the repository's release step"),
    e(Spawn, "steve-core/src/workspace.rs", 1, Later("E14 disk housekeeping (D13)"),
        "`lsof -d cwd` before caches are shed"),
    e(Dir, "steve-core/src/workspace.rs", 6, Later("E14 disk housekeeping (D13)"),
        "worktree cache sizes and the trash"),
    e(Dir, "steve-hub/src/api/projects.rs", 2, Later("projects describe and browse on the Node"),
        "the describe input, and browsing with no Node"),
    e(Spawn, "steve-core/src/release.rs", 2, Hub, "D12: the release script stays Hub-host for device=hub"),
    e(Fs, "steve-core/src/release.rs", 11, Hub, "D12: release logs and manifests in the Hub home"),
    /* Hub-host work */
    e(Spawn, "steve-core/src/lane/profile.rs", 1, Hub, "`gh api`"),
    e(Spawn, "steve-core/src/takeover.rs", 1, Hub, "`gh run list`"),
    e(Spawn, "steve-core/src/yolo/procs.rs", 1, Hub, "`ps`: the process table"),
    e(Spawn, "steve-hub/src/config.rs", 1, Hub, "npm config"),
    e(Spawn, "steve-hub/src/system.rs", 1, Hub, "host facts (sw_vers, sysctl, uname, hostname)"),
    e(Spawn, "steve-hub/src/upgrades/runner.rs", 6, Hub, "the upgrader"),
    e(Spawn, "steve-hub/src/upgrades/signing.rs", 2, Hub, "security -i: the signing keychain's new password"),
    e(Spawn, "steve-hub/src/upgrades/supervisor.rs", 2, Hub, "the supervisor starts the Hub, and asks a candidate's binary about this environment first"),
    e(Spawn, "steve-keyhelper/src/mac.rs", 2, Hub, "the keychain helper"),
    e(Spawn, "steve-modules/src/vault/keystore/helper.rs", 1, Hub, "the vault keystore helper"),
    e(Spawn, "steve-modules/src/net.rs", 1, Hub, "test support: reruns one test in a child"),
    e(Dir, "steve-hub/src/upgrades/mod.rs", 1, Hub, "upgrade operations in the Hub data dir"),
    e(Dir, "steve-modules/src/vault/inject.rs", 1, Hub, "the vault's scratch dirs in the Hub data dir"),
    e(Dir, "steve-knowledge/src/eval.rs", 1, Hub, "the offline retrieval eval corpus"),
    e(Fs, "steve-knowledge/src/eval.rs", 3, Hub, "the offline retrieval eval corpus"),
    e(Fs, "steve-auth/src/config.rs", 1, Hub, "the OIDC client secret file"),
    e(Fs, "steve-channels/src/cleanup.rs", 3, Hub, "the database backup before a purge, owner-only"),
    e(Fs, "steve-core/src/attachments.rs", 4, Hub, "thread attachments into the Hub's scratch dir"),
    e(Fs, "steve-core/src/orchestrator.rs", 1, Hub, "the scratch dir of an item with no project"),
    e(Fs, "steve-hub/src/api/files.rs", 1, Hub, "a blob download"),
    e(Fs, "steve-hub/src/app.rs", 1, Hub, "the upgrade control token"),
    e(Fs, "steve-hub/src/config.rs", 3, Hub, "the Hub's dirs and key files"),
    e(Fs, "steve-hub/src/files.rs", 5, Hub, "the blob store (atomic temp file + rename)"),
    e(Fs, "steve-hub/src/system.rs", 1, Hub, "host facts (/proc/meminfo)"),
    e(Fs, "steve-hub/src/upgrades/client.rs", 6, Hub, "the upgrader"),
    e(Fs, "steve-hub/src/upgrades/mod.rs", 11, Hub, "the upgrader"),
    e(Fs, "steve-hub/src/upgrades/runner.rs", 14, Hub, "the upgrader"),
    e(Fs, "steve-hub/src/upgrades/signing.rs", 4, Hub, "the signing keychain password file, moved into the key store; the rotation lock"),
    e(Fs, "steve-hub/src/upgrades/supervisor.rs", 12, Hub, "the supervisor"),
    e(Fs, "steve-hub/src/web.rs", 3, Hub, "the web assets"),
    e(Fs, "steve-hub/src/wire/node_files.rs", 4, Hub, "files a Node sends, into the blob store"),
    e(Fs, "steve-hub/src/wire/node_tools.rs", 2, Hub, "a Node's tool ledger lock in the Hub data dir"),
    e(Fs, "steve-knowledge/src/sources/files.rs", 1, Hub, "a blob read"),
    e(Fs, "steve-mail/src/attach.rs", 6, Hub, "mail attachments in the Hub home"),
    e(Fs, "steve-mail/src/config.rs", 1, Hub, "the test CA file"),
    e(Fs, "steve-mail/src/retention.rs", 1, Hub, "raw mail in the Hub home"),
    e(Fs, "steve-mail/src/send.rs", 1, Hub, "an attachment blob"),
    e(Fs, "steve-mail/src/store.rs", 3, Hub, "raw mail in the Hub home"),
    e(Fs, "steve-mail/src/threads.rs", 1, Hub, "raw mail in the Hub home"),
    e(Fs, "steve-mail/src/usage.rs", 1, Hub, "raw mail in the Hub home"),
    e(Fs, "steve-modules/src/vault/inject.rs", 4, Hub, "the vault's scratch dirs in the Hub data dir"),
    e(Fs, "steve-modules/src/vault/keystore/dir.rs", 1, Hub, "the vault keystore"),
    e(Fs, "steve-modules/src/vault/keystore/file.rs", 7, Hub, "the vault keystore"),
    e(Fs, "steve-node-wire/src/identity.rs", 3, Hub, "the Node link identity key"),
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
        .chain(scan::find_imports(sources, |flat| {
            rule.imports()
                .iter()
                .any(|(item, how)| scan::imports(flat, item, matches!(how, Any)))
        }))
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
    for rule in RULES {
        for s in sites(rule, krate, src) {
            found.entry((rule, s.file.clone())).or_default().push(s);
        }
    }
}

/// The guard itself: production code outside the Node starts harnesses and processes, lists
/// directories and touches files only where [`ALLOWED`] says.
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
        e(Harness, "steve-x/src/lib.rs", 1, NoNode, "t"),
        e(Dir, "steve-x/src/lib.rs", 2, Hub, "t"),
    ];
    assert!(check(&found, &listed).is_empty());
    // one site more than listed fails, and so does an entry with no site left
    let short = [
        e(Spawn, "steve-x/src/lib.rs", 1, Hub, "t"),
        e(Harness, "steve-x/src/lib.rs", 1, NoNode, "t"),
        e(Dir, "steve-x/src/lib.rs", 1, Hub, "t"),
        e(Spawn, "steve-y/src/gone.rs", 1, NoNode, "t"),
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

/// `use` trees flatten to one path per name, and an import counts when it hides the item.
#[test]
fn use_trees_flatten_and_match() {
    let code = "use a::{b, c::{self, d as e}, f::*};\npub(crate) use ::g::h;\n";
    let flat: Vec<String> = scan::uses(code).into_iter().map(|(_, p)| p).collect();
    assert_eq!(flat, ["a::b", "a::c", "a::c::d as e", "a::f::*", "g::h"]);
    let yes = |f, i, any| scan::imports(f, i, any);
    assert!(yes("steve_acp::rpc::Conn", "steve_acp::rpc", true));
    assert!(yes("steve_acp::*", "steve_acp::rpc", true));
    assert!(yes("steve_acp::rpc::*", "steve_acp::rpc", true));
    assert!(yes("steve_research as r", "steve_research::drive", true));
    assert!(!yes("steve_research", "steve_research::drive", true));
    assert!(!yes("steve_acp::normalize::Fact", "steve_acp::rpc", true));
    assert!(yes(
        "std::process::Command as Cmd",
        "std::process::Command",
        false
    ));
    assert!(!yes(
        "std::process::Command",
        "std::process::Command",
        false
    ));
    assert!(!yes(
        "std::process::Command as _",
        "std::process::Command",
        false
    ));
    assert!(!yes("std::process as p", "std::process::Command", false));
}

/// A test-only attribute on a field or argument ends at it: what follows is still read, and a
/// later `#[cfg(test)] mod tests` is still left out (wire/mod.rs had both).
#[test]
fn a_test_only_field_does_not_hide_what_follows() {
    let src = "fn f() -> S {\n    S {\n        a,\n        #[cfg(test)]\n        b,\n    }\n}\nfn g() { Command::new(\"x\"); }\n#[cfg(test)]\nmod tests {\n    fn t() { Command::new(\"y\"); }\n}\nfn h(#[cfg(test)] x: HashMap<A, B>, y: u8) { Command::new(\"z\"); }\n";
    let code = scan::production(src).unwrap();
    assert_eq!(scan::find_in(&code, "Command::new(").len(), 2, "{code}");
    assert!(!code.contains("fn t()"), "{code}");
    assert!(code.contains("a,") && !code.contains("b,"), "{code}");
    assert!(code.contains("y: u8"), "{code}");
}

/// A test-only file (`#![cfg(test)]`) is left out whole; another inner `cfg` is not.
#[test]
fn a_test_only_file_is_left_out() {
    assert!(scan::production("//! doc\n#![cfg(test)]\nfn f() { Command::new(\"x\"); }").is_none());
    assert!(scan::production("#![cfg(unix)]\nfn f() {}").is_some());
}

/// The review's evasions: each import form and each file operation below is a site, at its line.
#[test]
fn imports_and_file_operations_count() {
    let src = r#"
use steve_print::{PrintRun, run_supervised};
use steve_acp::{rpc::{Conn, Spawn}};
use std::process::{Command as Cmd};
use steve_research as r;
use std::fs::{self, remove_dir_all};
use tokio::fs::File as F;
use steve_core::thread::Researcher as _;
use steve_node_wire::{Conn as Link, ConnOptions};
use std::process::Command;
fn production(state: &State, project: &Path, worktree: &Path) {
    let _ = run_supervised(&run, &spawned, &on_message);
    let _ = Conn::spawn(sink, stream, generation, handler, opts);
    let _ = Cmd::new("claude");
    let _ = state.research.researcher.start(req);
    let _ = std::fs::write(project.join("x"), "");
    let _ = remove_dir_all(worktree);
    let _ = tokio::fs::File::open(project.join("y"));
    let _ = fs::create_dir_all(project);
    let _ = std::fs::remove_file(project.join("z"));
    let _ = std::fs::rename(project, worktree);
    let _ = std::fs::copy(project, worktree);
    let _ = std::fs::OpenOptions::new();
    let _ = std::fs::metadata(project);
}
"#;
    let s = vec![Source {
        rel: "steve-x/src/lib.rs".into(),
        code: scan::production(src).unwrap(),
    }];
    let lines =
        |rule| -> Vec<usize> { sites(rule, "steve-x", &s).iter().map(|s| s.line).collect() };
    // the PrintRun and run_supervised imports, the rpc Conn and Spawn imports, the steve_research
    // rename, the researcher's start; not the Node link's Conn, nor the core Researcher trait
    assert_eq!(lines(Rule::Harness), vec![2, 2, 3, 3, 5, 15]);
    // the renamed Command, not the plain import (its `Command::new(` is what counts)
    assert_eq!(lines(Rule::Spawn), vec![4]);
    // the imported remove_dir_all (its bare call is counted there) and the renamed File, then
    // every operation but metadata
    assert_eq!(lines(Rule::Fs), vec![6, 7, 16, 18, 19, 20, 21, 22, 23]);
    let mut found = BTreeMap::new();
    found_in("steve-x", &s, &mut found);
    assert_eq!(check(&found, &[]).len(), 3);
}
