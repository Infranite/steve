//! The Node boundary guard (N0e-E15 / #583): in production code, nothing outside the Node starts a
//! harness (the Claude/Codex/ACP adapters, `claude -p`, the research harness), starts a process or
//! capability subprocess, calls `steve_node::`, or touches project files and directories, except at
//! the reviewed Hub-host and library places in `node_boundary_host.txt`, each with its reason.
//!
//! It reads the sources of every workspace crate except the Node's own (`steve-node`,
//! `steve-node-cli`), following each crate's module tree from `src/lib.rs` and `src/main.rs` and
//! leaving out comments, literals and test-only items. Four things are counted per file:
//!
//! - [`Rule::Spawn`]: a process or confined capability subprocess started (`Command::new`,
//!   `Command::from`, `spawn_capability`, a renamed `Command`, `fork`/`exec`/`posix_spawn`).
//! - [`Rule::Harness`]: a harness execution entry point, a handle on one, or a direct `steve_node`
//!   call/import.
//! - [`Rule::Dir`]: a directory listing or a folder walk (`read_dir`, `scan::walk`).
//! - [`Rule::Fs`]: a file opened, read, written, copied, renamed, removed, or probed via
//!   metadata/canonicalize/existence/directory checks, or a directory created or removed
//!   (`std::fs`, `tokio::fs`, and `Path` probe methods).
//!
//! A call is found by its text, and an import by its item: `use a::{b::{c as d}}` counts as
//! `a::b::c as d`, so a braced, renamed or glob import of a listed item counts where the call
//! itself would not show (`run_supervised(`, `Cmd::new(`, `remove_dir_all(`). The guard is a
//! text scan, not a type check: it does not see a value that holds a harness passed elsewhere and
//! used there, only where such values are made or their modules imported.
//!
//! The inventory has two disjoint files (mirroring `layering.rs`):
//! - `tests/node_boundary_host.txt`: reviewed permanent Hub-host and harness-library sites
//!   ([`Until::Hub`] / [`Until::Library`]), matching [`RATIONALE`] 1:1. Never rewritten by
//!   `STEVE_GUARD_BLESS=1`; edits require explicit review.
//! - `tests/node_boundary.txt`: ratcheting debt inventory (target: 0). Any unlisted site lands
//!   here and fails the guard.

#[path = "support/ratchet.rs"]
mod ratchet;
#[path = "node_boundary/scan.rs"]
mod scan;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use scan::{Site, Source};

const HOST_LIST: &str = "tests/node_boundary_host.txt";
const DEBT_LIST: &str = "tests/node_boundary.txt";
const DEBT_HEADER: &str = "# Remaining Node boundary debt sites; only shrinks (target: 0).\n# rule<TAB>file under hub-rs/crates<TAB>count. Reviewed Hub-host/library sites live in node_boundary_host.txt.\n# STEVE_GUARD_BLESS=1 rewrites after removals and refuses growth.\n";

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
                "spawn_capability(",
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
                "steve_node::",
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
                "fs::metadata(",
                "fs::symlink_metadata(",
                "fs::canonicalize(",
                "fs::try_exists(",
                ".canonicalize()",
                ".try_exists()",
                ".exists()",
                ".is_dir()",
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
                ("steve_node", Any),
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
const FS_IMPORTS: [(&str, Import); 38] = [
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
    ("std::fs::metadata", Any),
    ("std::fs::symlink_metadata", Any),
    ("std::fs::canonicalize", Any),
    ("std::fs::try_exists", Any),
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
    ("tokio::fs::metadata", Any),
    ("tokio::fs::symlink_metadata", Any),
    ("tokio::fs::canonicalize", Any),
    ("tokio::fs::try_exists", Any),
    ("tokio::fs::symlink", Any),
];

/// Why a reviewed host/library entry exists. Project/session/MCP debt belongs on the Node, never
/// in a fallback variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Until {
    /// Permanent Hub-host work (Hub's own data, installer, upgrader, keychain, service supervision).
    Hub,
    /// Shared execution/harness libraries called by the Node; [`Rule::Harness`] bounds who else may
    /// call them.
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

use Rule::{Dir, Fs, Spawn};
use Until::{Hub, Library};

/// Reviewed reasons for every entry in `node_boundary_host.txt`.
#[rustfmt::skip]
const RATIONALE: &[Entry] = &[
    /* shared OS execution mechanism: Node runs and Hub-host capability servers */
    e(Spawn, "steve-confine/src/lib.rs", Library, "fresh login environment, confined host capability process (`spawn_capability`), and OS-confined pure-compute subprocess (--workflow-step); callers never construct these processes"),
    e(Spawn, "steve-confine/src/probe.rs", Library, "the single sandbox probe, with a minimal or control-plane policy on /usr/bin/true"),
    e(Dir, "steve-confine/src/control.rs", Library, "protected filename prefixes in the shared bubblewrap control-plane plan"),
    e(Fs, "steve-confine/src/control.rs", Library, "canonicalizes steve_home/node_home and checks run directories for the shared bubblewrap control-plane plan"),
    e(Fs, "steve-confine/src/probe.rs", Library, "checks whether the host sandbox executable (sandbox-exec / bwrap) exists"),
    e(Dir, "steve-confine/src/profile.rs", Library, "hide other entries when explicitly opening a harness credential store"),
    e(Fs, "steve-confine/src/profile.rs", Library, "canonicalizes sandbox mount paths and checks directory mounts when building Seatbelt/bwrap profiles"),
    e(Fs, "steve-confine/src/lib.rs", Library, "sandbox mount directories and a worktree's gitdir/commondir for its writable policy"),
    /* the harness libraries: what they start is reached only through the Node (Rule::Harness) */
    e(Spawn, "steve-acp/src/rpc.rs", Library, "an ACP adapter process"),
    e(Spawn, "steve-acp/src/config.rs", Library, "`npm root -g`, to find the adapters"),
    e(Fs, "steve-acp/src/config.rs", Library, "extensionless npm bin shebang check (`#!/usr/bin/env node`), so Node runs JS adapters directly"),
    e(Fs, "steve-acp/src/state_dir.rs", Library, "checks `$HOME/.gitconfig` existence when preparing a harness state directory on the Node"),
    e(Fs, "steve-acp/src/adapters.rs", Library, "adapter package.json `bin` entry resolution under `<steve-home>/adapters` or `npm root -g`, and the probe telling a missing adapter command from one that fails to start"),
    e(Dir, "steve-acp/src/adapter/claude_code.rs", Library, "Claude project transcript directory listing encapsulated in the adapter module"),
    e(Fs, "steve-acp/src/adapter/claude_code.rs", Library, "Claude transcript read/peek encapsulated in the adapter module"),
    e(Fs, "steve-acp/src/adapter/codex.rs", Library, "Codex rollout transcript read encapsulated in the adapter module"),
    e(Dir, "steve-acp/src/adapter/history.rs", Library, "sorted transcript directory listing shared by harness adapters in steve-acp"),
    e(Fs, "steve-acp/src/adapter/history.rs", Library, "shared transcript file read inside steve-acp"),
    e(Dir, "steve-toolchains/src/lib.rs", Library, "project marker listings for per-project discovery; Hub callers use only the data definitions"),
    e(Fs, "steve-toolchains/src/lib.rs", Library, "symlink-safe root and entry checks in per-project toolchain discovery; executed by the Node"),
    e(Dir, "steve-knowledge/src/scan.rs", Library, "the folder walk the Node's scan reuses"),
    e(Fs, "steve-knowledge/src/scan.rs", Library, "symlink canonicalization and file metadata in the folder walk library executed by the Node"),
    e(Spawn, "steve-acp/src/managed.rs", Library, "npm, to install and check the adapters Steve keeps (the Node's daily check, steve setup)"),
    e(Fs, "steve-acp/src/managed.rs", Library, "the installed adapters' package.json, selected ids, and own prefix directory/lease shared by Node execution and the operator CLI; never a workspace or client-chosen destination"),
    e(Dir, "steve-acp/src/rollout.rs", Library, "Codex rollout files: the Node's history, the stats repair"),
    e(Fs, "steve-acp/src/rollout.rs", Library, "Codex rollout files: the Node's history, the stats repair"),
    e(Dir, "steve-acp/src/transcript.rs", Library, "the shared ACP sampler reads the Node adapter's configured home: exact session transcript and its subagent files, with bounded discovery"),
    e(Fs, "steve-acp/src/transcript.rs", Library, "read-only assistant usage for that Node attempt, shared by sessions and one-shot calls; no Hub/project filesystem effects"),
    e(Fs, "steve-research/src/attach.rs", Library, "attachments in a research run's dir"),
    e(Fs, "steve-research/src/guard.rs", Library, "canonicalizes path prefixes inside the Node-side research path guard"),
    /* Hub-host process meters and metrics */
    e(Fs, "steve-hub/src/metrics.rs", Hub, "metrics socket mode 0600 and read-only /proc/self/io process counter on Linux; no workspace data"),
    e(Dir, "steve-hub/src/metrics/backup.rs", Hub, "discovers observations in the private installation directory used by offline backup and restore commands"),
    e(Fs, "steve-hub/src/metrics/backup.rs", Hub, "consumes decoded offline observations once as they enter the Hub metrics recorder"),
    e(Dir, "steve-runtime/src/process.rs", Library, "process meters: this process and its own process group (/proc on Linux, libproc on macOS); no workspace data"),
    e(Fs, "steve-runtime/src/process.rs", Library, "process meters: read-only counters of this process (/proc/self on Linux); no workspace data"),
    e(Spawn, "steve-runtime/src/process/bounded.rs", Library, "unified bounded subprocess runner rebuilding Command::new with a PATH-resolved absolute executable (preventing macOS std PATH fork fallback and running Linux DetachedCommand); callers are independently registered at their own Command/DetachedCommand entry points"),
    e(Fs, "steve-runtime/src/process/bounded.rs", Library, "PATH executable metadata lookup in resolve_program so bare program names resolve strictly against the child's effective PATH without macOS fork() fallback; no workspace or project data"),
    e(Spawn, "steve-runtime/src/process/macos_spawn.rs", Library, "unified macOS posix_spawn wrapper avoiding multithreaded fork() for detached-session probes and inherited-fd backup engine runs; callers are independently registered at their Command::new or engine entry points"),
    /* Hub-owned durable outputs, Hub-local temp-dir outputs, and releases */
    e(Dir, "steve-hub/src/api/outputs.rs", Hub, "Hub-owned durable `<home>/workspaces/<id>/outputs/` and Hub-local temp-dir output listing only; every project/worktree directory is read via the Node"),
    e(Fs, "steve-hub/src/api/outputs.rs", Hub, "Hub-owned durable `<home>/workspaces/<id>/outputs/` and Hub-local temp-dir output read only; every project/worktree file is read via the Node"),
    e(Spawn, "steve-core/src/release.rs", Hub, "D12: the release script stays Hub-host for device=hub"),
    e(Fs, "steve-core/src/release.rs", Hub, "D12: release logs and manifests in the Hub home"),
    /* Hub-host work */
    e(Spawn, "steve-hub/src/system.rs", Hub, "host facts (sw_vers, sysctl, uname, hostname)"),
    e(Fs, "steve-hub/src/nodes/fleet.rs", Hub, "reads `<data>/node/link-status.json` written by the colocated service Node"),
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
    e(Fs, "steve-install/src/service.rs", Hub, "operator service manager validates ownership, copies and verifies external Node update bundles into <home>/releases, reads existing registration without following links, archives only this installation's registration, and reads its own Hub's env and local Node pid file to stop that Node"),
    e(Spawn, "steve-install/src/service/boot.rs", Hub, "the explicit macOS Node boot command invokes a fixed administrator registration script; launchd's unprivileged supervisor runs and waits for only its selected Node child"),
    e(Fs, "steve-install/src/service/boot.rs", Hub, "operator-side macOS Node installer manages its own registration and locks, atomically commits or cleans up its staged locally signed boot supervisor, and opens only its own Node log"),
    e(Spawn, "steve-install/src/service/file_access.rs", Hub, "the operator-side macOS CLI opens a fixed System Settings pane with the user's consent"),
    e(Dir, "steve-install/src/service/file_access.rs", Hub, "operator-side macOS setup performs non-destructive TCC read_dir checks on Desktop/Documents/Downloads and configured roots when no running Node socket is up"),
    e(Fs, "steve-install/src/service/file_access.rs", Hub, "operator-side macOS setup reads this installation's own steve.env and roots.json to locate its socket and configured roots"),
    e(Dir, "steve-install/src/publish.rs", Hub, "steve publish packs exactly the folder the operator names, refusing links and special files instead of following them"),
    e(Fs, "steve-install/src/publish.rs", Hub, "steve login keeps its own client credential (0600, atomic replace); steve publish reads the file or folder the operator names to upload it"),
    e(Spawn, "steve-install/src/signing.rs", Hub, "first-install signer, trusted Hub updater, and macOS Node local self-signer invoke system openssl/security/codesign and verified build identity"),
    e(Dir, "steve-install/src/signing.rs", Hub, "signer validates only this release's regular-file tree and refuses preexisting Hub data"),
    e(Fs, "steve-install/src/signing.rs", Hub, "signer preserves the Hub identity and helper, manages the macOS Node's own local self-signed identity, keychain, private 0600 key-store password, and atomic signed binary/runner copies, reads the installation key-store selection, and hands its private staging password to the signed Hub on stdin before removing it"),
    e(Spawn, "steve-install/src/release.rs", Hub, "trusted updater uses the installation's environment, launchd service and candidate maintenance probe"),
    e(Dir, "steve-install/src/release.rs", Hub, "trusted updater inventories only the installation’s release receipts and retired bundles"),
    e(Fs, "steve-install/src/release.rs", Hub, "trusted updater uses the release lock, atomic links and protected deployment receipts"),
    e(Spawn, "steve-hub/src/upgrades/supervisor.rs", Hub, "the supervisor starts the Hub, verifies the candidate's sibling steve-node binary, and asks a candidate's binary about this environment first"),
    e(Spawn, "steve-keyhelper/src/mac.rs", Hub, "the keychain helper"),
    e(Spawn, "steve-modules/src/vault/keystore/helper.rs", Hub, "the vault keystore helper"),
    e(Spawn, "steve-modules/src/net.rs", Hub, "test support: reruns one test in a child"),
    e(Dir, "steve-hub/src/upgrades/mod.rs", Hub, "upgrade operations and release receipts in the Hub data dir"),
    e(Fs, "steve-store-sqlite/src/backup/offline.rs", Hub, "the SQLite driver stages a private online snapshot and verifies the checksum of a private restored copy"),
    e(Spawn, "steve-hub/src/backup/engine.rs", Hub, "runs the verified release's restic executable for the installation repository with a cleared environment and a pipe for its key (macOS delegates to steve_runtime::process::macos_spawn; non-macOS uses Command::new)"),
    e(Dir, "steve-hub/src/backup/engine.rs", Hub, "cleans the private backup staging directory while the installation repository lock is held"),
    e(Fs, "steve-hub/src/backup/engine.rs", Hub, "owns the private repository lock, cache and temporary staging lifecycle"),
    e(Dir, "steve-hub/src/backup/files.rs", Hub, "walks the Hub's allowlisted backup sources and excludes regenerable workspaces and key storage"),
    e(Fs, "steve-hub/src/backup/files.rs", Hub, "copies allowlisted installation files into private staging for stable restic paths"),
    e(Dir, "steve-hub/src/backup.rs", Hub, "measures total bytes in the installation-owned local repository for its capacity metric"),
    e(Fs, "steve-hub/src/backup.rs", Hub, "stages SQLite metadata and atomically replaces an offline database after verifying its restored snapshot"),
    e(Fs, "steve-hub/src/backup/repository.rs", Hub, "publishes the initialized repository and persists the intent to finalize or prune snapshots across interruption"),
    e(Fs, "steve-hub/src/admin/paths.rs", Hub, "the Hub's own management socket: its private 0700 run directory (or a per-user fallback under TMPDIR when the path is too long), the 0600 pointer file to it, removing only a stale socket of this user that no longer answers, and clearing the installation-owned startup readiness record"),
    e(Dir, "steve-knowledge/src/eval.rs", Hub, "the offline retrieval eval corpus"),
    e(Fs, "steve-knowledge/src/eval.rs", Hub, "the offline retrieval eval corpus"),
    e(Fs, "steve-knowledge/src/memory_file.rs", Hub, "atomic reads and writes of Hub-owned `$STEVE_HOME/context/` and owner-authorized DirectWrite personal memory files"),
    e(Fs, "steve-auth/src/config.rs", Hub, "shared configuration secret files"),
    e(Fs, "steve-core/src/attachments.rs", Hub, "thread attachments into the Hub's scratch dir"),
    e(Fs, "steve-core/src/orchestrator.rs", Hub, "the scratch dir of an item with no project and verification of a Hub-owned design report before approval"),
    e(Fs, "steve-hub/src/api/files.rs", Hub, "a blob download"),
    e(Fs, "steve-hub/src/app.rs", Hub, "the upgrade control token"),
    e(Spawn, "steve-hub/src/check.rs", Hub, "offline preflight delegates harness and Node.js discovery to the sibling `steve-node harness-probe` command"),
    e(Fs, "steve-hub/src/check.rs", Hub, "preflight checks the Hub's own STEVE_HOME/metrics directories and local tailcat availability"),
    e(Fs, "steve-hub/src/config.rs", Hub, "the Hub's dirs, steve.env, and harness manifest"),
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
    e(Fs, "steve-modules/src/vault/keystore/helper.rs", Hub, "checks the Hub's own `<STEVE_HOME>/bin/steve-keyhelper` executable permissions"),
    e(Fs, "steve-keyhelper/src/mac.rs", Hub, "canonicalizes old and current keyhelper binaries during macOS keychain migration"),
    e(Fs, "steve-node-wire/src/identity.rs", Hub, "the Node link identity key"),
    e(Fs, "steve-node-wire/src/tailcat.rs", Library, "checks optional `tailcat` binary metadata when locating the tunnel executable"),
    e(Fs, "steve-hub/src/harnesses.rs", Hub, "the Hub's own harness manifest, which the runtimes page adds adapters to"),
    e(Fs, "steve-core/src/config.rs", Hub, "locates Hub-host `gh`, running executable's own Steve checkout, and compares configured `STEVE_REPO`"),
    e(Fs, "steve-core/src/workspace.rs", Hub, "`is_disposable_dir` checks Hub-owned backup staging directories for regenerable cache directories"),
    e(Fs, "steve-hub/src/admin/routes.rs", Hub, "reports the Hub's own SQLite database file size in `/admin/status`"),
    e(Fs, "steve-hub/src/extension.rs", Hub, "resolves `<STEVE_HOME>/current` when syncing the Hub's own service definition on boot"),
    e(Fs, "steve-hub/src/upgrades/install.rs", Hub, "classifies the Hub's own installation/checkout layout and signing identity"),
    e(Fs, "steve-hub/src/upgrades/maintenance.rs", Hub, "resolves the Hub's own executable path for maintenance CLI commands"),
    e(Fs, "steve-install/src/main.rs", Hub, "operator CLI resolves its own executable bundle and `<home>/current` / `<home>/releases`"),
    e(Fs, "steve-install/src/setup/reconfigure/snapshot.rs", Hub, "canonicalizes the operator-selected vault key directory to keep it outside `$STEVE_HOME`"),
    e(Fs, "steve-knowledge/src/sources/folder.rs", Hub, "canonicalizes the Hub's own `$HOME` and `$STEVE_HOME/context` policy roots; user folder paths are probed exclusively by the Node"),
    e(Fs, "steve-store/src/db.rs", Hub, "checks whether the Hub's own SQLite database file exists in `inspect_schema`"),
];

/// The crates that are the Node.
const NODE: &[&str] = &["steve-node", "steve-node-cli"];
/// The crates that hold harness code; [`Rule::Harness`] is about their callers.
const LIBRARIES: &[&str] = &["steve-acp", "steve-research"];

/// Modules whose project/session/git/MCP operations must route strictly through `steve-node-api`
/// and never probe local paths, spawn capability subprocesses, or call `steve_node::`.
const PATH_PROBE_GUARDED_FILES: &[&str] = &[
    "steve-hub/src/resume.rs",
    "steve-hub/src/api/projects.rs",
    "steve-hub/src/api/project_setup.rs",
    "steve-hub/src/wire/library_mcp.rs",
    "steve-core/src/git.rs",
    "steve-core/src/cycle.rs",
    "steve-github/src/push.rs",
    "steve-github/src/repos.rs",
];

const PATH_PROBE_PATTERNS: &[&str] = &[
    ".is_dir(",
    ".exists(",
    "try_exists(",
    "fs::metadata(",
    "symlink_metadata(",
    "spawn_capability(",
    "steve_node::",
];

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

fn path_probe_violations(sources: &[Source]) -> Vec<Site> {
    let mut found: Vec<Site> = PATH_PROBE_PATTERNS
        .iter()
        .flat_map(|p| scan::find(sources, p))
        .collect();
    found.extend(scan::find_imports(sources, |flat| {
        scan::imports(flat, "steve_node", true)
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

#[test]
fn guarded_project_and_session_modules_never_probe_local_paths() {
    let all = sources();
    let by_rel: BTreeMap<&str, &Source> = all
        .iter()
        .flat_map(|(_, srcs)| srcs.iter())
        .map(|s| (s.rel.as_str(), s))
        .collect();
    let mut violations = Vec::new();
    for rel in PATH_PROBE_GUARDED_FILES {
        let src = by_rel
            .get(rel)
            .unwrap_or_else(|| panic!("guarded file not scanned: {rel}"));
        let probe = Source {
            rel: src.rel.clone(),
            code: src.code.clone(),
        };
        violations.extend(path_probe_violations(&[probe]));
    }
    assert!(
        violations.is_empty(),
        "Guarded Hub/Core/GitHub modules must route path checks, capability spawns, and Node calls via steve-node-api: {violations:?}"
    );
}

#[test]
fn local_path_probes_and_evasions_fail_the_guard() {
    let check = |raw| {
        path_probe_violations(&[Source {
            rel: "steve-hub/src/resume.rs".into(),
            code: scan::production(raw).unwrap(),
        }])
    };
    for raw in [
        "fn f(p: &Path) { let _ = p.is_dir(); }",
        "fn f(p: &Path) { let _ = p.exists(); }",
        "async fn f(p: &Path) { let _ = tokio::fs::try_exists(p).await; }",
        "fn f(p: &Path) { let _ = std::fs::metadata(p); }",
        "fn f(p: &Path) { let _ = std::fs::symlink_metadata(p); }",
        "fn f() { let _ = Sandbox::spawn_capability(cmd); }",
        "fn f() { let _ = steve_node::project_setup::clone(req); }",
        "use steve_node::LocalNode;",
    ] {
        assert!(!check(raw).is_empty(), "missed {raw}");
    }
    assert!(
        check(
            r#"
        // Path::is_dir() and steve_node::project_setup in comments do not count.
        fn f() { let _ = "p.exists() spawn_capability("; }
        #[cfg(test)] fn t(p: &Path) { let _ = p.is_dir(); let _ = steve_node::LocalNode::open; }
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

fn locations(found: &BTreeMap<(Rule, String), Vec<Site>>) -> BTreeMap<Vec<String>, String> {
    found
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
        .collect()
}

// The fixtures use the same count comparison as the on-disk inventory.
fn check(found: &BTreeMap<(Rule, String), Vec<Site>>, allowed: &ratchet::Counts) -> Vec<String> {
    let locs = locations(found);
    ratchet::differences_with(&inventory(found), allowed, "go through the Node", |key| {
        locs.get(key).cloned().unwrap_or_else(|| key.join(" "))
    })
}

fn partition(
    now: ratchet::Counts,
    host: &ratchet::Counts,
    debt: &ratchet::Counts,
) -> (ratchet::Counts, ratchet::Counts) {
    for key in debt.keys() {
        assert!(
            !host.contains_key(key),
            "node boundary key in both host and debt inventories: {key:?}"
        );
    }
    let mut host_now = ratchet::Counts::new();
    let mut debt_now = ratchet::Counts::new();
    for (key, count) in now {
        if host.contains_key(&key) {
            host_now.insert(key, count);
        } else {
            debt_now.insert(key, count);
        }
    }
    (host_now, debt_now)
}

fn found_in(krate: &str, src: &[Source], found: &mut BTreeMap<(Rule, String), Vec<Site>>) {
    for rule in RULES {
        for s in sites(rule, krate, src) {
            found.entry((rule, s.file.clone())).or_default().push(s);
        }
    }
}

/// The guard itself: production code outside the Node starts harnesses and processes, lists
/// directories and touches files only at reviewed host/library places in `node_boundary_host.txt`
/// (with zero debt in `node_boundary.txt`).
#[test]
fn nothing_outside_the_node_starts_a_harness_or_walks_a_project_unlisted() {
    let mut found = BTreeMap::new();
    for (krate, src) in sources() {
        found_in(&krate, &src, &mut found);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let host_file = root.join(HOST_LIST);
    let debt_file = root.join(DEBT_LIST);
    let host_allowed = ratchet::read(&host_file, 2);
    let debt_allowed = ratchet::read(&debt_file, 2);
    let (host_now, debt_now) = partition(inventory(&found), &host_allowed, &debt_allowed);
    let locs = locations(&found);

    let host_problems = ratchet::differences_with(
        &host_now,
        &host_allowed,
        "review host/library boundary; edit node_boundary_host.txt and RATIONALE explicitly",
        |key| locs.get(key).cloned().unwrap_or_else(|| key.join(" ")),
    );
    assert!(
        host_problems.is_empty(),
        "Host/library boundary changed; edit {HOST_LIST} explicitly:\n{}",
        host_problems.join("\n")
    );

    let debt_problems =
        ratchet::differences_with(&debt_now, &debt_allowed, "go through the Node", |key| {
            locs.get(key).cloned().unwrap_or_else(|| key.join(" "))
        });
    if !debt_problems.is_empty() {
        eprintln!(
            "Node boundary debt locations:\n{}",
            debt_problems.join("\n")
        );
    }
    ratchet::check(
        debt_now,
        &debt_file,
        2,
        DEBT_HEADER,
        "go through the Node; reviewed Hub-host/library work belongs in node_boundary_host.txt + RATIONALE",
    );
}

#[test]
fn host_and_debt_inventories_are_disjoint_and_unlisted_sites_fail_debt() {
    let host = ratchet::Counts::from([(vec!["Fs".into(), "steve-hub/src/files.rs".into()], 1)]);
    let debt = ratchet::Counts::new();
    let dir = tempfile::tempdir().unwrap();
    let debt_file = dir.path().join("node_boundary.txt");
    ratchet::check_at(
        debt.clone(),
        &debt_file,
        2,
        DEBT_HEADER,
        "fix",
        ratchet::Mode::Init,
    );

    let (h, d) = partition(host.clone(), &host, &debt);
    assert_eq!(h, host);
    assert!(d.is_empty());

    let mut with_unlisted = host.clone();
    with_unlisted.insert(vec!["Spawn".into(), "steve-hub/src/rogue.rs".into()], 1);
    let (_, d) = partition(with_unlisted, &host, &debt);
    assert!(!ratchet::growth(&d, &debt).is_empty());

    assert!(std::panic::catch_unwind(|| partition(ratchet::Counts::new(), &host, &host)).is_err());
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

/// Each reviewed host/library entry says why and names a file that is there, outside the Node, and
/// debt stays empty.
#[test]
fn every_allowed_entry_has_a_reason_and_a_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let host = ratchet::read(&root.join(HOST_LIST), 2);
    let debt = ratchet::read(&root.join(DEBT_LIST), 2);
    assert!(
        debt.is_empty(),
        "node_boundary.txt debt must remain empty: {debt:?}"
    );
    let problems = rationale_problems(&host, RATIONALE);
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
    let _ = steve_confine::Sandbox::spawn_capability(cmd);
    let _ = steve_node::LocalNode::open(cfg);
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
    assert_eq!(lines(Rule::Spawn), vec![6, 9]);
    assert_eq!(lines(Rule::Harness), vec![7, 10]);
    assert_eq!(lines(Rule::Dir), vec![8, 21]);
    // a harness library's own entry points are not its callers
    assert!(sites(Rule::Harness, "steve-acp", &s).is_empty());

    let mut found = BTreeMap::new();
    found_in("steve-x", &s, &mut found);
    // unlisted: all three fail
    let p = check(&found, &ratchet::Counts::new());
    assert_eq!(p.len(), 3, "{p:?}");
    assert!(
        p.iter()
            .any(|l| l.starts_with("Spawn steve-x/src/lib.rs:6,9: found 2, listed 0")),
        "{p:?}"
    );
    // listed: passes
    let listed = ratchet::Counts::from([
        (vec!["Spawn".into(), "steve-x/src/lib.rs".into()], 2),
        (vec!["Harness".into(), "steve-x/src/lib.rs".into()], 2),
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
            .any(|l| l.starts_with("Dir steve-x/src/lib.rs:8,21: found 2, listed 1"))
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
    assert!(yes("steve_node::LocalNode", "steve_node", true));
    assert!(yes("steve_node as n", "steve_node", true));
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
    let _ = std::fs::symlink_metadata(project);
    let _ = project.canonicalize();
    let _ = project.exists();
    let _ = project.is_dir();
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
    // every file operation including metadata, symlink_metadata, canonicalize, exists, and is_dir
    assert_eq!(
        lines(Rule::Fs),
        vec![6, 7, 16, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28]
    );
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
