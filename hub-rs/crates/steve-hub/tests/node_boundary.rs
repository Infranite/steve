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
//! - [`Rule::Harness`]: a harness execution entry point or a handle on one.
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
//! Every count must equal its entry in `node_boundary.txt`; [`RATIONALE`] supplies reasons. A new call site fails the test: route it
//! through the Node (a `WorkspaceExecutor`, `Discovery` or job face), or, if it is truly Hub-host
//! work (the Hub's own data, the upgrader, the keychain, `gh` API calls), add it to the table with
//! why. A removed call site fails too, so the table shrinks with the code. Harness execution
//! runs on the Node. Those marked [`Until::NoNode`] serve items no Node holds, including on
//! a diagnostic Hub, and [`Until::Later`] names the follow-up that moves the rest.

#[path = "support/ratchet.rs"]
mod ratchet;
#[path = "node_boundary/scan.rs"]
mod scan;

use std::collections::{BTreeMap, BTreeSet};
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
                "steve_acp::oneshot",
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
                ("steve_acp::oneshot", Any),
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
    /// Hub-host access for items no Node holds, including on a diagnostic Hub.
    NoNode,
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
    until: Until,
    why: &'static str,
}

const fn e(rule: Rule, file: &'static str, until: Until, why: &'static str) -> Entry {
    Entry {
        rule,
        file,
        until,
        why,
    }
}

use Rule::{Dir, Fs, Harness, Spawn};
use Until::{Hub, Later, Library, NoNode};

/// Reasons and removal milestones for the count inventory in node_boundary.txt.
#[rustfmt::skip]
const RATIONALE: &[Entry] = &[
    /* shared OS execution mechanism: Node runs and Hub-host capability servers */
    e(Spawn, "steve-confine/src/lib.rs", Library, "fresh login environment and confined host capability process; callers never construct these processes"),
    e(Spawn, "steve-confine/src/probe.rs", Library, "the single sandbox probe, with a minimal or control-plane policy on /usr/bin/true"),
    e(Dir, "steve-confine/src/control.rs", Library, "protected filename prefixes in the shared bubblewrap control-plane plan"),
    e(Dir, "steve-confine/src/profile.rs", Library, "hide other entries when explicitly opening a harness credential store"),
    e(Fs, "steve-confine/src/lib.rs", Library, "sandbox mount directories and a worktree's gitdir/commondir for its writable policy"),
    /* the harness libraries: what they start is reached only through the Harness entries */
    e(Spawn, "steve-acp/src/rpc.rs", Library, "an ACP adapter process"),
    e(Spawn, "steve-acp/src/config.rs", Hub, "`npm root -g`, to find the adapters"),
    e(Fs, "steve-acp/src/config.rs", Library, "extensionless npm bin shebang check (`#!/usr/bin/env node`), so Node runs JS adapters directly"),
    e(Fs, "steve-acp/src/adapters.rs", Library, "adapter package.json `bin` entry resolution under `<steve-home>/adapters` or `npm root -g`"),
    e(Dir, "steve-toolchains/src/lib.rs", Library, "project marker listings for per-project discovery; Hub callers use only the data definitions"),
    e(Dir, "steve-knowledge/src/scan.rs", Library, "the folder walk the Node's scan reuses"),
    e(Spawn, "steve-acp/src/managed.rs", Library, "npm, to install and check the adapters Steve keeps (the Node's daily check, steve setup)"),
    e(Fs, "steve-acp/src/managed.rs", Library, "the installed adapters' package.json, selected ids, and own prefix directory/lease shared by Node execution and the operator CLI; never a workspace or client-chosen destination"),
    e(Dir, "steve-acp/src/rollout.rs", Library, "Codex rollout files: the Node's history, the stats repair"),
    e(Fs, "steve-acp/src/rollout.rs", Library, "Codex rollout files: the Node's history, the stats repair"),
    e(Dir, "steve-acp/src/transcript.rs", Library, "the shared ACP sampler reads the Node adapter's configured home: exact session transcript and its subagent files, with bounded discovery"),
    e(Fs, "steve-acp/src/transcript.rs", Library, "read-only assistant usage for that Node attempt, shared by sessions and one-shot calls; no Hub/project filesystem effects"),
    e(Fs, "steve-research/src/attach.rs", Library, "attachments in a research run's dir"),
    /* harness entry points outside the Node */
    e(Harness, "steve-hub/src/wire/research.rs", Hub,
        "the steve_research import: the run's policy (its attempts, its answer); the runs are Node jobs"),
    e(Fs, "steve-hub/src/metrics.rs", Hub, "metrics socket mode 0600 and read-only /proc/self/io process counter on Linux; no workspace data"),
    e(Dir, "steve-runtime/src/process.rs", Library, "process meters: this process and its own process group (/proc on Linux, libproc on macOS); no workspace data"),
    e(Fs, "steve-runtime/src/process.rs", Library, "process meters: read-only counters of this process (/proc/self on Linux); no workspace data"),
    /* project directories on the Hub host */
    e(Spawn, "steve-core/src/git.rs", NoNode, "git for items no Node holds"),
    e(Fs, "steve-core/src/git.rs", NoNode, "locking the repository's common .git directory around git worktree changes for items no Node holds"),
    e(Spawn, "steve-github/src/push.rs", NoNode, "the Hub-side push when no Node holds the item"),
    e(Dir, "steve-hub/src/api/outputs.rs", NoNode, "the outputs listing with no Node"),
    e(Dir, "steve-knowledge/src/sources/folder.rs", NoNode, "the knowledge folder walk with no Node"),
    e(Fs, "steve-hub/src/api/outputs.rs", NoNode, "an output read from disk when no Node holds the item"),
    e(Fs, "steve-knowledge/src/sources/folder.rs", NoNode, "a knowledge folder file read with no Node"),
    e(Fs, "steve-core/src/cycle.rs", NoNode, "the worktree fingerprint for items no Node holds (its throwaway index and the lock a killed git leaves)"),
    e(Fs, "steve-github/src/push.rs", NoNode, "the Hub-side push borrows the project's git objects"),
    e(Fs, "steve-core/src/workspace.rs", Later("E14 disk housekeeping (D13)"),
        "workspace folders created and moved to the trash"),
    e(Fs, "steve-hub/src/api/projects.rs", Later("projects describe and browse on the Node"),
        "the README read for the describe input"),
    e(Fs, "steve-core/src/orchestrator.rs", Later("the queue's cwd check from the Node"),
        "the write probe in a session's cwd"),
    e(Spawn, "steve-github/src/repos.rs", Later("a project's remote from Node git facts"),
        "`git remote get-url` of a project"),
    e(Spawn, "steve-core/src/workspace.rs", Later("E14 disk housekeeping (D13)"),
        "`lsof -d cwd` before caches are shed"),
    e(Dir, "steve-core/src/workspace.rs", Later("E14 disk housekeeping (D13)"),
        "worktree cache sizes and the trash"),
    e(Dir, "steve-hub/src/api/projects.rs", Later("projects describe and browse on the Node"),
        "the describe input and the new-project directory browser (projects are made on the Hub's own Node only)"),
    e(Spawn, "steve-core/src/release.rs", Hub, "D12: the release script stays Hub-host for device=hub"),
    e(Fs, "steve-core/src/release.rs", Hub, "D12: release logs and manifests in the Hub home"),
    /* Hub-host work */
    e(Spawn, "steve-hub/src/config.rs", Hub, "npm config"),
    e(Spawn, "steve-hub/src/system.rs", Hub, "host facts (sw_vers, sysctl, uname, hostname)"),
    e(Fs, "steve-hub/src/nodes/mod.rs", Hub, "the Hub's own identity key directory"),
    e(Spawn, "steve-hub/src/wire/local_service.rs", Hub, "starting the Hub's own Node in a process of its own (B1)"),
    e(Fs, "steve-hub/src/wire/local_service.rs", Hub, "the service Node's key, link file, pid file and log in its home (B1)"),
    e(Fs, "steve-hub/src/wire/host_node.rs", Hub, "the roots file the service Node reads (B1)"),
    e(Spawn, "steve-hub/src/nodes/tailcat.rs", Hub, "tailcat genkey and serve: the Hub's own tunnel for remote Nodes"),
    e(Fs, "steve-hub/src/nodes/tailcat.rs", Hub, "the Hub's tailcat key directory"),
    e(Spawn, "steve-hub/src/nodes/install.rs", Hub, "ssh-keygen, the per-install SSH tunnel, and its commands: the Hub installing a Node on another machine"),
    e(Spawn, "steve-hub/src/plugins/run.rs", Hub, "a connected channel plugin: the Hub's own channel service, never a task harness"),
    e(Dir, "steve-hub/src/plugins/mod.rs", Hub, "the installed plugins under <home>/plugins"),
    e(Fs, "steve-hub/src/plugins/mod.rs", Hub, "atomic writes, reads and removal of installed plugins, their theme manifests and connected instances under <home>/plugins"),
    e(Fs, "steve-hub/src/plugins/install.rs", Hub, "a plugin archive checked against its library's sha256 and unpacked under <home>/plugins"),
    e(Fs, "steve-hub/src/plugins/run.rs", Hub, "a running plugin's own files, data directory and bounded log under <home>/plugins"),
    e(Fs, "steve-hub/src/nodes/install.rs", Hub, "the install's own SSH key (removed on cancellation too), a local release directory, the bounded cache of checked installer archives under <home>/tailcat/installs/cache, and its tests' fixtures"),
    e(Dir, "steve-hub/src/nodes/install.rs", Hub, "the installer archive cache, to keep only the most recently used"),
    e(Fs, "steve-hub/src/nodes/upgrade.rs", Hub, "a Node upgrade's archive under <data>/node-upgrades, built from the Hub's own release's Node and maintenance CLI; the manifest is read once"),
    e(Spawn, "steve-hub/src/upgrades/runner.rs", Hub, "the upgrader (launchctl); published bundles prepare and deploy in native Rust"),
    e(Spawn, "steve-hub/src/upgrades/cli.rs", Hub, "OS-owner upgrade CLI queries the signed Hub's service and code identity, never a task harness"),
    e(Dir, "steve-hub/src/upgrades/cli.rs", Hub, "OS-owner upgrade CLI lists this installation's operation journals only"),
    e(Fs, "steve-hub/src/upgrades/cli.rs", Hub, "OS-owner upgrade CLI validates this installation's owned receipts, journals and service definition"),
    e(Spawn, "steve-i18n/src/lib.rs", Hub, "`defaults read -g AppleLanguages`: the operator CLI's language follows the macOS system languages when no locale variable names one"),
    e(Spawn, "steve-install/src/common.rs", Hub, "operator CLI invokes only verified bundle binaries for identity, configuration, enrollment and maintenance"),
    e(Dir, "steve-install/src/common.rs", Hub, "operator installer inventories its immutable package and private release copies, not projects"),
    e(Fs, "steve-install/src/common.rs", Hub, "operator installer owns private setup receipts and verified package copies"),
    e(Dir, "steve-install/src/setup.rs", Hub, "first installation checks an explicitly selected vault-key directory is empty before owning it"),
    e(Spawn, "steve-install/src/setup.rs", Hub, "the wizard's last step opens the Hub's own address in the operator's browser (open, xdg-open)"),
    e(Fs, "steve-install/src/setup.rs", Hub, "first installation validates, copies and registers only its own release and configuration under a setup lock, and reads back the owner token it saved; a fresh installation renames the old home, and the key directory its receipt names, aside under the same lock, and the last step reads this installation's own address"),
    e(Spawn, "steve-install/src/setup/reconfigure.rs", Hub, "operator configuration maintenance runs only the verified installed Hub's bounded configuration check"),
    e(Fs, "steve-install/src/setup/reconfigure.rs", Hub, "operator configuration maintenance reads private configuration and locks, syncs and archives its own recoverable transaction"),
    e(Spawn, "steve-install/src/service.rs", Hub, "operator service manager invokes launchctl/systemctl, verified installation identity commands, and fixed administrator commands only for explicit macOS Node lifecycle actions"),
    e(Fs, "steve-install/src/service.rs", Hub, "operator service manager validates ownership, reads existing registration without following links, archives only this installation's registration, and reads its own Hub's env and local Node pid file to stop that Node"),
    e(Spawn, "steve-install/src/service/boot.rs", Hub, "the explicit macOS Node boot command invokes a fixed administrator registration script; launchd's unprivileged supervisor runs and waits for only its selected Node child"),
    e(Fs, "steve-install/src/service/boot.rs", Hub, "operator-side macOS Node installer manages its own registration and locks, retires the verified old definition, atomically commits or cleans up its staged locally signed boot supervisor, and opens only its own Node log"),
    e(Dir, "steve-install/src/service/boot.rs", Hub, "one-time macOS LaunchAgent migration enumerates only this installation's private release directory to recover a verified release when no running job can attest to one"),
    e(Spawn, "steve-install/src/service/file_access.rs", Hub, "the operator-side macOS CLI opens a fixed System Settings pane with the user's consent; the running Node performs all protected-folder probes"),
    e(Dir, "steve-install/src/publish.rs", Hub, "steve publish packs exactly the folder the operator names, refusing links and special files instead of following them"),
    e(Fs, "steve-install/src/publish.rs", Hub, "steve login keeps its own client credential (0600, atomic replace); steve publish reads the file or folder the operator names to upload it"),
    e(Spawn, "steve-install/src/signing.rs", Hub, "first-install signer, trusted Hub updater, and macOS Node local self-signer invoke system openssl/security/codesign and verified build identity"),
    e(Dir, "steve-install/src/signing.rs", Hub, "signer validates only this release's regular-file tree and refuses preexisting Hub data"),
    e(Fs, "steve-install/src/signing.rs", Hub, "signer preserves the Hub identity and helper, manages the macOS Node's own local self-signed identity, keychain, private 0600 key-store password, and atomic signed binary/runner copies, reads the installation key-store selection, and hands its private staging password to the signed Hub on stdin before removing it"),
    e(Spawn, "steve-install/src/release.rs", Hub, "trusted updater uses the installation's environment, launchd service and candidate maintenance probe"),
    e(Dir, "steve-install/src/release.rs", Hub, "trusted updater inventories owned backups, release receipts and failed probe database files"),
    e(Fs, "steve-install/src/release.rs", Hub, "trusted updater uses the release lock, verified SQLite backups, atomic links and protected rollback receipts"),
    e(Spawn, "steve-hub/src/upgrades/supervisor.rs", Hub, "the supervisor starts the Hub, and asks a candidate's binary about this environment first"),
    e(Spawn, "steve-keyhelper/src/mac.rs", Hub, "the keychain helper"),
    e(Spawn, "steve-modules/src/vault/keystore/helper.rs", Hub, "the vault keystore helper"),
    e(Spawn, "steve-modules/src/net.rs", Hub, "test support: reruns one test in a child"),
    e(Dir, "steve-hub/src/upgrades/mod.rs", Hub, "upgrade operations in the Hub data dir"),
    e(Fs, "steve-store-sqlite/src/backup/offline.rs", Hub, "the single backup implementation copies the Hub's own database file into its backups directory: a 0700 directory, a create_new 0600 partial, fsync, and a no-replace hard-link publish (replaces the daily copy's own file handling)"),
    e(Dir, "steve-store-sqlite/src/backup/plan.rs", Hub, "inventories installation-owned backup directories and their declared daily file trees for retention accounting"),
    e(Fs, "steve-store-sqlite/src/backup/plan.rs", Hub, "stages private backup directories, verifies and publishes manifests atomically under the backup-root lock, and retires declared artifacts and file trees with the manifest last"),
    e(Dir, "steve-hub/src/admin/backup.rs", Hub, "daily snapshots and tar archives walk only the installation parts of their backup scope, and measure skipped workspace caches for the log"),
    e(Fs, "steve-hub/src/admin/backup.rs", Hub, "archives the Hub-owned backup scope or copies it into the daily tree; hard links reuse data only from the preceding backup, never from live installation files"),
    e(Fs, "steve-hub/src/admin/paths.rs", Hub, "the Hub's own management socket: its private 0700 run directory (or a per-user fallback under TMPDIR when the path is too long), the 0600 pointer file to it, removing only a stale socket of this user that no longer answers, and clearing the installation-owned startup readiness record"),
    e(Dir, "steve-knowledge/src/eval.rs", Hub, "the offline retrieval eval corpus"),
    e(Fs, "steve-knowledge/src/eval.rs", Hub, "the offline retrieval eval corpus"),
    e(Fs, "steve-auth/src/config.rs", Hub, "shared configuration secret files"),
    e(Fs, "steve-core/src/attachments.rs", Hub, "thread attachments into the Hub's scratch dir"),
    e(Fs, "steve-core/src/orchestrator.rs", Hub, "the scratch dir of an item with no project and verification of a Hub-owned design report before approval"),
    e(Fs, "steve-hub/src/api/files.rs", Hub, "a blob download"),
    e(Fs, "steve-hub/src/app.rs", Hub, "the upgrade control token"),
    e(Fs, "steve-hub/src/config.rs", Hub, "the Hub's dirs and harness manifest"),
    e(Fs, "steve-hub/src/files.rs", Hub, "the blob store (atomic temp file + rename and no-follow integrity read)"),
    e(Fs, "steve-hub/src/system.rs", Hub, "host facts (/proc/meminfo)"),
    e(Fs, "steve-hub/src/upgrades/client.rs", Hub, "the upgrader"),
    e(Fs, "steve-hub/src/upgrades/mod.rs", Hub, "the upgrader"),
    e(Fs, "steve-hub/src/upgrades/runner.rs", Hub, "the upgrader"),
    e(Fs, "steve-hub/src/upgrades/supervisor.rs", Hub, "the supervisor"),
    e(Fs, "steve-hub/src/web.rs", Hub, "the web assets"),
    e(Fs, "steve-hub/src/access.rs", Hub, "the single sign-on client secret the settings keep in the Hub's own data directory"),
    e(Fs, "steve-hub/src/wire/node_files.rs", Hub, "granted attachment reads; writes use the shared blob store"),
    e(Fs, "steve-hub/src/deliveries/mod.rs", Hub,
        "immutable objects owned by the Hub, synced before publishing a delivery row; sources are read through steve-node and archive names are never filesystem paths"),
    e(Fs, "steve-store/src/contract/tool_journal.rs", Hub, "the host tool ledger lock stays in the Hub data directory with the journal it fences"),
    e(Fs, "steve-knowledge/src/sources/files.rs", Hub, "a blob read"),
    e(Fs, "steve-mail/src/attach.rs", Hub, "mail attachments in the Hub home"),
    e(Fs, "steve-mail/src/config.rs", Hub, "the test CA file"),
    e(Fs, "steve-mail/src/retention.rs", Hub, "raw mail in the Hub home"),
    e(Fs, "steve-mail/src/send.rs", Hub, "an attachment blob"),
    e(Fs, "steve-mail/src/store.rs", Hub, "raw mail in the Hub home"),
    e(Fs, "steve-mail/src/threads.rs", Hub, "raw mail in the Hub home"),
    e(Fs, "steve-mail/src/usage.rs", Hub, "raw mail in the Hub home"),
    e(Fs, "steve-modules/src/vault/keystore/dir.rs", Hub, "the vault keystore"),
    e(Fs, "steve-modules/src/vault/keystore/file.rs", Hub, "the vault keystore"),
    e(Fs, "steve-node-wire/src/identity.rs", Hub, "the Node link identity key"),
    e(Fs, "steve-hub/src/harnesses.rs", Hub, "the Hub's own harness manifest, which the runtimes page adds adapters to"),
];

/// The crates that are the Node.
const NODE: &[&str] = &["steve-node", "steve-node-cli"];
/// The crates that hold harness code; [`Rule::Harness`] is about their callers.
const LIBRARIES: &[&str] = &["steve-acp", "steve-research"];

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

/// Runtime sandbox assembly belongs to the Node. Hub-host capabilities use Sandbox or
/// confinement instead; moving implementation into a library must not widen that boundary.
const RUN_SANDBOX_INTERNALS: &[&str] = &[
    "wrap_in_mode",
    "wrap_readonly",
    "bwrap_readonly_standalone",
    "Places",
    "LoginStore",
];

fn hub_run_sandbox_internals(sources: &[Source]) -> Vec<Site> {
    let mut found = Vec::new();
    for source in sources {
        for name in RUN_SANDBOX_INTERNALS {
            for at in scan::find_in(&source.code, name) {
                if source
                    .code
                    .as_bytes()
                    .get(at + name.len())
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    continue;
                }
                found.push(Site {
                    file: source.rel.clone(),
                    line: source.code[..at].bytes().filter(|b| *b == b'\n').count() + 1,
                });
            }
        }
    }
    found.extend(scan::find_imports(sources, |flat| {
        RUN_SANDBOX_INTERNALS
            .iter()
            .any(|name| scan::imports(flat, &format!("steve_confine::{name}"), true))
    }));
    found.sort();
    found.dedup();
    found
}

#[test]
fn the_hub_uses_host_capabilities_not_run_sandbox_internals() {
    let sources = scan::crate_sources(crates_dir(), "steve-hub");
    let found = hub_run_sandbox_internals(&sources);
    assert!(
        found.is_empty(),
        "Hub must use Sandbox/confinement: {found:?}"
    );
}

#[test]
fn run_sandbox_internals_cannot_hide_in_imports_or_method_calls() {
    let check = |raw| {
        hub_run_sandbox_internals(&[Source {
            rel: "steve-hub/src/example.rs".into(),
            code: scan::production(raw).unwrap(),
        }])
    };
    for raw in [
        "fn f() { c.wrap_in_mode(args); }",
        "fn f() { c.wrap_readonly(args); }",
        "fn f() { steve_confine::bwrap_readonly_standalone(args); }",
        "use steve_confine::{Places as Writable, LoginStore};",
        "use steve_confine::*;",
        "use steve_confine as c;",
    ] {
        assert!(!check(raw).is_empty(), "missed {raw}");
    }
    assert!(
        check(
            r#"
        use steve_confine::{Sandbox, confinement};
        // LoginStore and wrap_readonly are documentation, not capabilities.
        fn f() { let s = "Places"; let other = PlacesElsewhere; }
        #[cfg(test)] fn t() { c.wrap_in_mode(args); }
    "#
        )
        .is_empty()
    );
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

fn inventory(found: &BTreeMap<(Rule, String), Vec<Site>>) -> ratchet::Counts {
    found
        .iter()
        .map(|((rule, file), sites)| (vec![format!("{rule:?}"), file.clone()], sites.len()))
        .collect()
}

// The fixtures use the same count comparison as the on-disk inventory.
fn check(found: &BTreeMap<(Rule, String), Vec<Site>>, allowed: &ratchet::Counts) -> Vec<String> {
    let locations: BTreeMap<_, _> = found
        .iter()
        .map(|((rule, file), sites)| {
            let lines: BTreeSet<_> = sites.iter().map(|site| site.line).collect();
            let lines = lines
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(",");
            (
                vec![format!("{rule:?}"), file.clone()],
                format!("{rule:?} {file}:{lines}"),
            )
        })
        .collect();
    ratchet::differences_with(&inventory(found), allowed, "go through the Node", |key| {
        locations.get(key).cloned().unwrap_or_else(|| key.join(" "))
    })
}

fn found_in(krate: &str, src: &[Source], found: &mut BTreeMap<(Rule, String), Vec<Site>>) {
    for rule in RULES {
        for s in sites(rule, krate, src) {
            found.entry((rule, s.file.clone())).or_default().push(s);
        }
    }
}

/// The guard itself: production code outside the Node starts harnesses and processes, lists
/// directories and touches files only where [`RATIONALE`] says.
#[test]
fn nothing_outside_the_node_starts_a_harness_or_walks_a_project_unlisted() {
    let mut found = BTreeMap::new();
    for (krate, src) in sources() {
        found_in(&krate, &src, &mut found);
    }
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/node_boundary.txt");
    if file.exists() {
        let listed = ratchet::read(&file, 2);
        let problems = check(&found, &listed);
        if !problems.is_empty() {
            eprintln!("Node boundary source locations:\n{}", problems.join("\n"));
        }
    }
    ratchet::check(
        inventory(&found),
        &file,
        2,
        "# Existing Node boundary sites; only shrinks.\n# rule<TAB>file under hub-rs/crates<TAB>count. Reasons stay in node_boundary.rs.\n# STEVE_GUARD_BLESS=1 rewrites after removals.\n",
        "go through the Node; a reviewed new entry needs a reason in RATIONALE",
    );
}

fn rationale_problems(listed: &ratchet::Counts, reasons: &[Entry]) -> Vec<String> {
    let keys: BTreeSet<_> = listed.keys().cloned().collect();
    let rationale: BTreeSet<_> = reasons
        .iter()
        .map(|entry| vec![format!("{:?}", entry.rule), entry.file.to_owned()])
        .collect();
    keys.difference(&rationale)
        .map(|key| format!("missing reason: {}", key.join(" ")))
        .chain(
            rationale
                .difference(&keys)
                .map(|key| format!("stale rationale: {}; remove it", key.join(" "))),
        )
        .collect()
}

/// Each entry says why and names a file that is there, outside the Node.
#[test]
fn every_allowed_entry_has_a_reason_and_a_file() {
    let listed = ratchet::read(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/node_boundary.txt"),
        2,
    );
    let problems = rationale_problems(&listed, RATIONALE);
    assert!(
        problems.is_empty(),
        "Node boundary rationale:\n{}",
        problems.join("\n")
    );
    for a in RATIONALE {
        assert!(!a.why.trim().is_empty(), "{}: no reason", a.file);
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
    let p = check(&found, &ratchet::Counts::new());
    assert_eq!(p.len(), 3, "{p:?}");
    assert!(
        p.iter()
            .any(|l| l.starts_with("Spawn steve-x/src/lib.rs:6: found 1, listed 0")),
        "{p:?}"
    );
    // listed: passes
    let listed = ratchet::Counts::from([
        (vec!["Spawn".into(), "steve-x/src/lib.rs".into()], 1),
        (vec!["Harness".into(), "steve-x/src/lib.rs".into()], 1),
        (vec!["Dir".into(), "steve-x/src/lib.rs".into()], 2),
    ]);
    assert!(check(&found, &listed).is_empty());
    // One site more than listed fails, as does an entry with no site left.
    let mut short = listed;
    short.insert(vec!["Dir".into(), "steve-x/src/lib.rs".into()], 1);
    short.insert(vec!["Spawn".into(), "steve-y/src/gone.rs".into()], 1);
    let p = check(&found, &short);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(
        p.iter()
            .any(|l| l.starts_with("Dir steve-x/src/lib.rs:8,19: found 2, listed 1"))
    );
    assert!(
        p.iter()
            .any(|l| l.starts_with("Spawn steve-y/src/gone.rs: found 0, listed 1"))
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
use steve_acp::oneshot::{Request, run};
use steve_acp::{rpc::{Conn, Spawn}};
use std::process::{Command as Cmd};
use steve_research as r;
use std::fs::{self, remove_dir_all};
use tokio::fs::File as F;
use steve_core::thread::Researcher as _;
use steve_node_wire::{Conn as Link, ConnOptions};
use std::process::Command;
fn production(state: &State, project: &Path, worktree: &Path) {
    let _ = run(req, &spawned, &on_event);
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
    // the oneshot path and its Request and run imports, the rpc Conn and Spawn imports, the steve_research
    // rename, the researcher's start; not the Node link's Conn, nor the core Researcher trait
    assert_eq!(lines(Rule::Harness), vec![2, 2, 2, 3, 3, 5, 15]);
    // the renamed Command, not the plain import (its `Command::new(` is what counts)
    assert_eq!(lines(Rule::Spawn), vec![4]);
    // the imported remove_dir_all (its bare call is counted there) and the renamed File, then
    // every operation but metadata
    assert_eq!(lines(Rule::Fs), vec![6, 7, 16, 18, 19, 20, 21, 22, 23]);
    let mut found = BTreeMap::new();
    found_in("steve-x", &s, &mut found);
    assert_eq!(check(&found, &ratchet::Counts::new()).len(), 3);
}

#[test]
fn rationale_and_inventory_must_match_in_both_directions() {
    let listed = ratchet::Counts::from([(vec!["Spawn".into(), "fixture.rs".into()], 1)]);
    let reasons = [e(Spawn, "fixture.rs", Hub, "fixture")];
    assert!(rationale_problems(&listed, &reasons).is_empty());
    assert_eq!(
        rationale_problems(&listed, &[]),
        ["missing reason: Spawn fixture.rs"]
    );
    assert_eq!(
        rationale_problems(&ratchet::Counts::new(), &reasons),
        ["stale rationale: Spawn fixture.rs; remove it"]
    );
    let wrong_rule = [e(Fs, "fixture.rs", Hub, "different rule")];
    assert_eq!(rationale_problems(&listed, &wrong_rule).len(), 2);
}
