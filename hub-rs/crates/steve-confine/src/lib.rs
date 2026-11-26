//! Shared OS sandbox generation and probing for Node runs, Research, Vault and Hub capabilities.
//! The OS sandbox around what a Node starts for a run: a session's harness, a job's harness, a
//! command. This is the first stage of "every run in the OS sandbox" (path model S12). Steve's
//! control plane stays out of the run's reach whatever the harness does with its own shell: the
//! Hub's and the Node's secrets, keys, databases and journal cannot be read or written, and
//! Steve's own programs cannot be changed (`{PROTECTED, NODE_PROTECTED,
//! NODE_FILES, SEALED}`). A deny-by-default profile built from the run's view comes later.
//!
//! macOS uses `sandbox-exec` (seatbelt) with deny rules, so everything else is as before. Linux
//! uses `bwrap` (bubblewrap) as an allowlist: all of `STEVE_HOME` is hidden but the run
//! directories, which is stricter (Steve's programs and logs are not readable there either, and
//! what a run writes elsewhere in it goes to an empty tmpfs that is gone with the run). The Node tries the sandbox
//! once when it opens. When it is missing, fails, or `STEVE_CONFINE=off`, runs go unconfined and the
//! Node says so once at open: starting a run never fails because of this stage. A harness that runs
//! its own seatbelt sandbox (an adapter declaring `nested_sandbox: false`) is not wrapped, because
//! macOS refuses a sandbox inside a sandbox; it keeps its own.
//!
//! Levels (`STEVE_CONFINE`, C1):
//! - `0` (`off`): no sandbox.
//! - `1` (`on`, the default): the above, and the file system isolated for
//!   a run in a workspace: it writes only there (and the repository a worktree belongs to), in its
//!   harness and toolchain directories, the temporary directories, the usual tool caches and the
//!   harnesses' own homes, plus `STEVE_CONFINE_WRITABLE` (`:`-separated). Reads stay open but for
//!   Steve's own. The environment is left as it is: what the user's system sets, the run gets.
//! - `2`: level 1, and a fresh environment: what a new login shell of the user starts with (asked
//!   once of `$SHELL -l`), plus what Steve sets for the run, nothing inherited from the Node.

use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

mod control;
mod probe;
mod profile;
pub use control::{
    BwrapPlan, NODE_FILES, NODE_PROTECTED, PROTECTED, RUN_DIRS, SEALED, bwrap_plan, canonical_path,
};
pub use probe::{available, bwrap_path, bwrap_works, seatbelt_works};
#[cfg(test)]
use profile::*;
pub use profile::{
    pure_compute_bwrap_args, pure_compute_seatbelt_profile, readonly_profile, seatbelt_rules,
};
/// Home-relative secrets denied to read-only jobs, shared with Research's tool guard.
pub const SENSITIVE_HOME_DIRS: [&str; 5] =
    [".ssh", ".steve", ".aws", ".config/gh", "Library/Keychains"];

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The sandbox of this Node's runs: which one, how far it isolates files, and the environment
/// runs start from at level 2.
#[derive(Debug, Clone)]
pub struct Confine {
    sandbox: Backend,
    /// Writes of a run in a workspace are kept to its own places (level 1 and up).
    files: bool,
    /// Level 2: the user's fresh login environment.
    env: Option<std::sync::Arc<Vec<(OsString, OsString)>>>,
}

#[derive(Debug, Clone)]
enum Backend {
    /// Runs go unconfined: why.
    Off(String),
    /// macOS: the rules to append to a profile.
    Seatbelt(String),
    /// Linux: what to hide and seal is listed again for each run (a name prefix matches the files
    /// there now).
    Bwrap {
        bwrap: PathBuf,
        steve_home: PathBuf,
        node_home: PathBuf,
    },
}

impl Confine {
    /// The sandbox for runs of a Node with these homes, tried once on `/usr/bin/true`.
    pub fn open(steve_home: &Path, node_home: &Path) -> Confine {
        let c = Self::from_env(steve_home, node_home);
        let level = match (&c.sandbox, c.files, c.env.is_some()) {
            (Backend::Off(_), ..) => 0,
            (_, _, true) => 2,
            (_, true, _) => 1,
            _ => 0,
        };
        match &c.sandbox {
            Backend::Off(why) => tracing::warn!("node: runs are not confined: {why}"),
            Backend::Seatbelt(_) => {
                tracing::info!("node: runs are confined (seatbelt, level {level})")
            }
            Backend::Bwrap { .. } => {
                tracing::info!("node: runs are confined (bubblewrap, level {level})")
            }
        }
        c
    }

    /// Level 2: the environment a run starts from (then what Steve sets for it).
    pub fn fresh_env(&self) -> Option<&[(OsString, OsString)]> {
        self.env.as_deref().map(Vec::as_slice)
    }

    fn from_env(steve_home: &Path, node_home: &Path) -> Confine {
        Self::choose(steve_home, node_home, std::env::var("STEVE_CONFINE").ok())
    }

    pub fn choose(steve_home: &Path, node_home: &Path, mode: Option<String>) -> Confine {
        let off = |why: String| Confine {
            sandbox: Backend::Off(why),
            files: false,
            env: None,
        };
        let level = match mode.as_deref().map(str::trim) {
            Some("off") | Some("0") => {
                return off(format!("STEVE_CONFINE={}", mode.unwrap_or_default().trim()));
            }
            None | Some("") | Some("on") | Some("1") => 1,
            Some("2") => 2,
            Some(other) => {
                return off(format!("STEVE_CONFINE={other} is not 0, 1, 2, on or off"));
            }
        };
        let sandbox = Self::sandbox(steve_home, node_home);
        if matches!(sandbox, Backend::Off(_)) {
            return Confine {
                sandbox,
                files: false,
                env: None,
            };
        }
        Confine {
            sandbox,
            files: level >= 1,
            env: (level >= 2).then(|| std::sync::Arc::new(login_env())),
        }
    }

    fn sandbox(steve_home: &Path, node_home: &Path) -> Backend {
        probe::detect(Some((steve_home, node_home))).unwrap_or_else(Backend::Off)
    }

    /// Whether runs are confined.
    #[cfg(test)]
    pub fn on(&self) -> bool {
        !matches!(self.sandbox, Backend::Off(_))
    }

    /// The sandbox runs get (`seatbelt`, `bubblewrap`), or why they get none.
    pub fn state(&self) -> Result<&'static str, String> {
        match &self.sandbox {
            Backend::Off(why) => Err(why.clone()),
            Backend::Seatbelt(_) => Ok("seatbelt"),
            Backend::Bwrap { .. } => Ok("bubblewrap"),
        }
    }

    /// The seatbelt rules, to append to a profile a run already has (a job's or research's own).
    pub fn seatbelt_rules(&self) -> Option<&str> {
        match &self.sandbox {
            Backend::Seatbelt(r) => Some(r),
            _ => None,
        }
    }

    /// A principal uses linked credentials. Seatbelt can let the owner also renew them
    /// in place without making the rest of the directory writable. Bubblewrap's store
    /// mount masks other entries, so keep that mount for non-owner sessions only.
    pub fn opens_principal_login_store(&self, hide_cli_creds: bool) -> bool {
        hide_cli_creds || matches!(self.sandbox, Backend::Seatbelt(_))
    }

    /// `program args` run in the sandbox; `readable`: directories it may read although they are
    /// under what is hidden (a command's own secret files).
    pub fn wrap(
        &self,
        program: PathBuf,
        args: Vec<OsString>,
        readable: &[PathBuf],
    ) -> (PathBuf, Vec<OsString>) {
        self.wrap_in(program, args, readable, &[], None)
    }

    /// [`Self::wrap`] for a run that works in `cwd`: bubblewrap changes into it itself, so a
    /// directory the sandbox hides fails the start instead of leaving the run in `$HOME`.
    /// `writable`: directories it may read and write although they are under what is hidden (a
    /// session's own harness directory, under the Node's `state/`).
    pub fn wrap_in(
        &self,
        program: PathBuf,
        args: Vec<OsString>,
        readable: &[PathBuf],
        writable: &[PathBuf],
        cwd: Option<&Path>,
    ) -> (PathBuf, Vec<OsString>) {
        self.wrap_in_mode(
            program, args, readable, writable, cwd, false, false, None, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn wrap_in_mode(
        &self,
        program: PathBuf,
        args: Vec<OsString>,
        readable: &[PathBuf],
        writable: &[PathBuf],
        cwd: Option<&Path>,
        isolated_state: bool,
        hide_cli_creds: bool,
        user_home: Option<&Path>,
        login: Option<&LoginStore>,
    ) -> (PathBuf, Vec<OsString>) {
        // the places a run in a workspace writes, level 1 and up
        let writes = match cwd {
            Some(cwd) if self.files => {
                Some(writable_places(cwd, writable, isolated_state, user_home))
            }
            _ => None,
        };
        let hidden_cli = if hide_cli_creds {
            user_home.map(cli_credential_paths).unwrap_or_default()
        } else {
            Vec::new()
        };
        match &self.sandbox {
            Backend::Off(_) => (program, args),
            Backend::Seatbelt(rules) => {
                let profile = profile::seatbelt_run(
                    rules,
                    writes.as_ref(),
                    &hidden_cli,
                    readable,
                    writable,
                    login,
                );
                let mut a = vec!["-p".into(), profile.into(), program.into()];
                a.extend(args);
                (SANDBOX_EXEC.into(), a)
            }
            Backend::Bwrap {
                bwrap,
                steve_home,
                node_home,
            } => {
                if node_home.starts_with(steve_home.join("data")) {
                    for d in RUN_DIRS {
                        let _ = std::fs::create_dir_all(steve_home.join(d));
                    }
                }
                for d in NODE_PROTECTED {
                    if let Some(d) = d.strip_suffix('/') {
                        let _ = std::fs::DirBuilder::new()
                            .recursive(true)
                            .mode(0o700)
                            .create(node_home.join(d));
                    }
                }
                let plan = bwrap_plan(steve_home, node_home);
                let a = profile::bwrap_run(
                    &plan,
                    writes.as_ref(),
                    &hidden_cli,
                    readable,
                    writable,
                    cwd,
                    login,
                    program,
                    args,
                );
                (bwrap.clone(), a)
            }
        }
    }

    /// Wraps a read-only job (`acp_think` or `research`) on Linux using Bubblewrap with
    /// `--ro-bind / /`, control-plane hiding, sensitive home masking, and `--bind` only on
    /// `writable` (the run's scratch dir and declared harness state dirs).
    pub fn wrap_readonly(
        &self,
        program: PathBuf,
        args: Vec<OsString>,
        writable: &[PathBuf],
        chdir: Option<&Path>,
        user_home: &Path,
    ) -> (PathBuf, Vec<OsString>) {
        match &self.sandbox {
            Backend::Bwrap {
                bwrap,
                steve_home,
                node_home,
            } => bwrap_readonly_with_plan(
                bwrap,
                Some((steve_home, node_home)),
                program,
                args,
                writable,
                chdir,
                user_home,
            ),
            _ => {
                if let Some(bwrap) = bwrap_path() {
                    bwrap_readonly_standalone(&bwrap, program, args, writable, chdir, user_home)
                } else {
                    (program, args)
                }
            }
        }
    }
}

pub fn is_outer_sandbox_active(has_readonly_sandbox: bool, confine: Option<&Confine>) -> bool {
    has_readonly_sandbox || confine.is_some_and(|c| !matches!(c.sandbox, Backend::Off(_)))
}

pub fn bwrap_readonly_standalone(
    bwrap: &Path,
    program: PathBuf,
    args: Vec<OsString>,
    writable: &[PathBuf],
    chdir: Option<&Path>,
    user_home: &Path,
) -> (PathBuf, Vec<OsString>) {
    bwrap_readonly_with_plan(bwrap, None, program, args, writable, chdir, user_home)
}

fn bwrap_readonly_with_plan(
    bwrap: &Path,
    homes: Option<(&Path, &Path)>,
    program: PathBuf,
    args: Vec<OsString>,
    writable: &[PathBuf],
    chdir: Option<&Path>,
    user_home: &Path,
) -> (PathBuf, Vec<OsString>) {
    let plan = homes.map(|(steve, node)| bwrap_plan(steve, node));
    let args = profile::bwrap_readonly(plan.as_ref(), program, args, writable, chdir, user_home);
    (bwrap.to_path_buf(), args)
}

/// The built-in harnesses' own places under the owner's home (logins, tokens, history), hidden
/// from a session of another principal, whose harness keeps its state in its own directory.
fn cli_credential_paths(user_home: &Path) -> Vec<PathBuf> {
    let (dirs, files) = harness_home_places();
    dirs.iter()
        .chain(files.iter())
        .map(|rel| user_home.join(rel))
        .collect()
}

/// The Node's credential store of a harness a session uses although it may not see the Node's
/// harness directory ([`steve_harness_spec::Login`]): the directory, and the names in it the harness reads
/// and writes, renewing its sign-in included (a trailing `*` matches any rest of a name). So a
/// renewal is never made and then lost, which would spend the Node's sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginStore {
    pub dir: PathBuf,
    pub names: Vec<String>,
}

impl LoginStore {
    fn admits(&self, name: &str) -> bool {
        self.names.iter().any(|n| match n.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => n == name,
        })
    }
}

/// Where a run in a workspace may write (level 1 and up).
#[derive(Debug, Default)]
pub struct Places {
    pub dirs: Vec<PathBuf>,
    /// Single files (the harness's own settings file in the home).
    pub files: Vec<PathBuf>,
}

/// Toolchain and package caches under the user's home a run may write regardless of principal
/// isolation: what compilers and package managers write as a matter of course.
const HOME_CACHES: &[&str] = &[
    ".cache",
    "Library/Caches",
    ".npm",
    ".pnpm-store",
    "Library/pnpm",
    ".local/share/pnpm",
    ".yarn",
    ".bun",
    ".deno",
    ".cargo",
    ".rustup",
    "go",
    ".gradle",
    ".m2",
    ".nuget",
    ".gem",
    ".cocoapods",
];

/// What else a run in ambient mode (the owner's own harness state, `!isolated_state`) writes
/// under the home: the configuration and state roots tools write as a matter of course.
const HOME_AMBIENT: &[&str] = &[".local/state", ".config"];

/// Where the built-in harnesses keep their state under the user's home, as their adapters declare
/// it (`StateDir::home_places`): directories, then single files, home-relative.
fn harness_home_places() -> (Vec<String>, Vec<String>) {
    let (mut dirs, mut files) = (Vec::<String>::new(), Vec::<String>::new());
    for builtin in steve_harness_spec::builtins() {
        if let Some(sd) = &builtin.properties.state {
            let (d, f) = sd.home_places();
            for x in d {
                if !dirs.iter().any(|y| y == x) {
                    dirs.push(x.to_string());
                }
            }
            for x in f {
                if !files.iter().any(|y| y == x) {
                    files.push(x.to_string());
                }
            }
        }
    }
    (dirs, files)
}

fn writable_places(
    cwd: &Path,
    writable: &[PathBuf],
    isolated_state: bool,
    user_home: Option<&Path>,
) -> Places {
    writable_places_with(cwd, writable, isolated_state, user_home, &|k| {
        std::env::var_os(k)
    })
}

fn writable_places_with(
    cwd: &Path,
    writable: &[PathBuf],
    isolated_state: bool,
    user_home: Option<&Path>,
    var: &impl Fn(&str) -> Option<OsString>,
) -> Places {
    let mut dirs: Vec<PathBuf> = vec![cwd.to_path_buf()];
    // a worktree's commits go into the repository it belongs to
    if let Some(common) = git_common_dir(cwd) {
        dirs.push(common);
    }
    dirs.extend(writable.iter().cloned());
    for t in ["/tmp", "/private/tmp", "/var/tmp", "/private/var/folders"] {
        dirs.push(PathBuf::from(t));
    }
    if let Some(t) = var("TMPDIR") {
        dirs.push(PathBuf::from(t));
    }
    let mut files = vec![];
    let home = user_home
        .map(Path::to_path_buf)
        .or_else(|| var("HOME").map(PathBuf::from));
    if let Some(home) = home {
        dirs.extend(HOME_CACHES.iter().map(|d| home.join(d)));
        if !isolated_state {
            let (harness_dirs, harness_files) = harness_home_places();
            dirs.extend(HOME_AMBIENT.iter().map(|d| home.join(d)));
            dirs.extend(harness_dirs.iter().map(|d| home.join(d)));
            files.extend(harness_files.iter().map(|f| home.join(f)));
        }
    }
    if let Some(extra) = var("STEVE_CONFINE_WRITABLE") {
        dirs.extend(std::env::split_paths(&extra).filter(|p| p.is_absolute()));
    }
    Places { dirs, files }
}

/// The `.git` directory a worktree's `.git` file points into (`gitdir: <repo>/.git/worktrees/x`).
fn git_common_dir(cwd: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(cwd.join(".git")).ok()?;
    let gitdir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        cwd.join(gitdir)
    };
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => gitdir.join(c.trim()),
        Err(_) => gitdir.parent()?.parent()?.to_path_buf(),
    };
    std::fs::canonicalize(&common).ok().or(Some(common))
}

/// What a new login shell of this user starts with (level 2): `$SHELL -l` from an empty
/// environment but for who the user is. A shell that cannot say gives a plain one.
fn login_env() -> Vec<(OsString, OsString)> {
    let keep = ["HOME", "USER", "LOGNAME", "SHELL", "TMPDIR"];
    let base: Vec<(String, OsString)> = keep
        .iter()
        .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v)))
        .collect();
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let out = std::process::Command::new(&shell)
        .args(["-l", "-c", "env -0"])
        .env_clear()
        .envs(base.iter().map(|(k, v)| (k, v)))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TERM", "xterm-256color")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let mut env: Vec<(OsString, OsString)> = match out {
        Ok(o) if o.status.success() => o
            .stdout
            .split(|b| *b == 0)
            .filter_map(|kv| {
                let kv = std::str::from_utf8(kv).ok()?;
                let (k, v) = kv.split_once('=')?;
                // the shell's own bookkeeping is not the user's environment
                (!k.is_empty() && !matches!(k, "_" | "SHLVL" | "PWD" | "OLDPWD"))
                    .then(|| (OsString::from(k), OsString::from(v)))
            })
            .collect(),
        _ => vec![],
    };
    for (k, v) in base {
        if !env.iter().any(|(e, _)| e == k.as_str()) {
            env.push((k.into(), v));
        }
    }
    if !env.iter().any(|(k, _)| k == "PATH") {
        env.push((
            "PATH".into(),
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into(),
        ));
    }
    if !env.iter().any(|(k, _)| k == "LANG") {
        env.push(("LANG".into(), "en_US.UTF-8".into()));
    }
    env
}
/// Whether runs use the system sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// Use it when the platform has it; otherwise rely on layers 1 and 2 (logged once at startup).
    #[default]
    Auto,
    /// Refuse to run without it.
    Required,
    /// Never (tests of the other layers only).
    Off,
}

/// Whether a run under `mode` is sandboxed; `None` when `Required` but unavailable.
pub fn decide(mode: SandboxMode) -> Option<bool> {
    match mode {
        SandboxMode::Off => Some(false),
        SandboxMode::Auto => Some(available()),
        SandboxMode::Required => available().then_some(true),
    }
}

/// The OS sandbox a Node with these homes and this `STEVE_CONFINE` would give its runs
/// (`seatbelt`, `bubblewrap`), or why none: what `steve-hub --check-config` reports before a switch.
pub fn confinement(
    steve_home: &Path,
    node_home: &Path,
    mode: Option<String>,
) -> Result<&'static str, String> {
    Confine::choose(steve_home, node_home, mode).state()
}

/// A sandbox for programs the Hub itself starts outside any run (the library's stdio MCP servers it
/// proxies): the same one this Node's runs get, so what such a program does cannot reach Steve's
/// secrets, keys and databases either. Without a sandbox here, programs start as they are.
#[derive(Clone)]
pub struct Sandbox(Confine);

/// Extra OS CPU-time headroom (seconds) above the wall-clock timeout so QuickJS's own interrupt
/// handler fires cleanly before the kernel sends `SIGXCPU`/`SIGKILL`.
pub const PURE_COMPUTE_CPU_HEADROOM_SECS: u64 = 2;

/// Maximum open file descriptors for a pure-compute workflow step subprocess (stdin/stdout/stderr
/// plus dynamic loader headroom).
pub const PURE_COMPUTE_MAX_OPEN_FILES: u64 = 64;

/// Maximum regular file size writable by a pure-compute subprocess (`0` bytes).
pub const PURE_COMPUTE_MAX_FILE_SIZE_BYTES: u64 = 0;

/// Maximum core dump size for a pure-compute subprocess (`0` bytes).
pub const PURE_COMPUTE_MAX_CORE_BYTES: u64 = 0;

/// Virtual address-space ceiling applied on Linux (`512 MiB`, well above QuickJS's 32 MiB heap and
/// the `steve-hub` executable mapping, while capping native allocation bombs).
pub const PURE_COMPUTE_LINUX_AS_BYTES: u64 = 512 * 1024 * 1024;

/// OS resource limits (`setrlimit`) enforced on pure-compute workflow step subprocesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PureComputeLimits {
    pub cpu_seconds: u64,
    pub max_file_size_bytes: u64,
    pub max_core_bytes: u64,
    pub max_open_files: u64,
    pub max_address_space_bytes: Option<u64>,
}

impl PureComputeLimits {
    /// Builds conservative OS rlimits derived from the step's wall-clock budget.
    pub fn for_wall_time(wall_time: std::time::Duration) -> Self {
        Self {
            cpu_seconds: wall_time
                .as_secs()
                .max(1)
                .saturating_add(PURE_COMPUTE_CPU_HEADROOM_SECS),
            max_file_size_bytes: PURE_COMPUTE_MAX_FILE_SIZE_BYTES,
            max_core_bytes: PURE_COMPUTE_MAX_CORE_BYTES,
            max_open_files: PURE_COMPUTE_MAX_OPEN_FILES,
            #[cfg(target_os = "linux")]
            max_address_space_bytes: Some(PURE_COMPUTE_LINUX_AS_BYTES),
            #[cfg(not(target_os = "linux"))]
            max_address_space_bytes: None,
        }
    }

    /// Applies these resource limits to the current process (used both in `pre_exec` and at the
    /// start of `steve-hub --workflow-step`). Clamps requested limits to existing hard ceilings so
    /// unprivileged processes never fail with `EPERM`.
    pub fn apply_self(&self) {
        apply_rlimit_capped(libc::RLIMIT_CPU, self.cpu_seconds);
        apply_rlimit_capped(libc::RLIMIT_FSIZE, self.max_file_size_bytes);
        apply_rlimit_capped(libc::RLIMIT_CORE, self.max_core_bytes);
        apply_rlimit_capped(libc::RLIMIT_NOFILE, self.max_open_files);
        if let Some(as_bytes) = self.max_address_space_bytes {
            apply_rlimit_capped(libc::RLIMIT_AS, as_bytes);
        }
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
type RlimitResource = libc::c_int;

fn apply_rlimit_capped(resource: RlimitResource, limit: u64) {
    // SAFETY: `getrlimit`/`setrlimit` operate solely on a stack-allocated POD `rlimit` struct
    // with no heap allocations or locks (async-signal-safe for `pre_exec`).
    unsafe {
        let mut cur = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(resource, &mut cur) != 0 {
            return;
        }
        let target = limit as libc::rlim_t;
        let next_max = if cur.rlim_max == libc::RLIM_INFINITY {
            target
        } else {
            cur.rlim_max.min(target)
        };
        let next = libc::rlimit {
            rlim_cur: next_max,
            rlim_max: next_max,
        };
        let _ = libc::setrlimit(resource, &next);
    }
}

fn standalone_pure_compute_backend() -> &'static Option<StandalonePureBackend> {
    static DETECTED: std::sync::OnceLock<Option<StandalonePureBackend>> =
        std::sync::OnceLock::new();
    DETECTED.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            if seatbelt_works() {
                return Some(StandalonePureBackend::Seatbelt);
            }
        }
        #[cfg(target_os = "linux")]
        {
            // `bwrap_path()` already runs the shared `probe::detect(None)` availability check.
            if let Some(bwrap) = bwrap_path() {
                return Some(StandalonePureBackend::Bwrap(bwrap));
            }
        }
        None
    })
}

enum StandalonePureBackend {
    #[cfg(target_os = "macos")]
    Seatbelt,
    #[cfg(target_os = "linux")]
    Bwrap(PathBuf),
}

impl Sandbox {
    /// The sandbox for a Steve with these homes, tried once (`STEVE_CONFINE` as for runs).
    pub fn open(steve_home: &Path, node_home: &Path) -> Sandbox {
        Sandbox(Confine::open(steve_home, node_home))
    }

    /// The sandbox selected for an explicit `STEVE_CONFINE` mode string (tests and diagnostics).
    pub fn choose(steve_home: &Path, node_home: &Path, mode: Option<String>) -> Sandbox {
        Sandbox(Confine::choose(steve_home, node_home, mode))
    }

    /// The active OS sandbox backend (`seatbelt`, `bubblewrap`), or why runs would be unconfined.
    pub fn state(&self) -> Result<&'static str, String> {
        self.0.state()
    }

    /// Whether an OS sandbox backend is active.
    pub fn is_confined(&self) -> bool {
        self.0.state().is_ok()
    }

    /// Starts a pure-compute subprocess (`steve-hub --workflow-step`) inside a read-only,
    /// no-network OS sandbox (`seatbelt` on macOS, `bubblewrap` on Linux) with an empty temporary
    /// `cwd`, cleared environment, process-group SIGKILL cleanup on Drop, and OS `setrlimit` caps.
    pub fn spawn_pure_compute(
        sandbox: Option<&Self>,
        program: PathBuf,
        args: Vec<std::ffi::OsString>,
        limits: PureComputeLimits,
    ) -> std::io::Result<CapabilityProcess> {
        let cwd = tempfile::Builder::new()
            .prefix("steve-wf-step-")
            .tempdir()?;
        let user_home = std::env::var_os("HOME").map(PathBuf::from);
        let (wrapped_program, wrapped_args) = match sandbox.map(|s| &s.0.sandbox) {
            Some(Backend::Seatbelt(rules)) => {
                let profile =
                    profile::pure_compute_seatbelt_with_rules(Some(rules), user_home.as_deref());
                let mut a = vec!["-p".into(), profile.into(), program.into()];
                a.extend(args);
                (PathBuf::from(SANDBOX_EXEC), a)
            }
            Some(Backend::Bwrap {
                bwrap,
                steve_home,
                node_home,
            }) => {
                let plan = bwrap_plan(steve_home, node_home);
                let a = profile::pure_compute_bwrap_args(
                    Some(&plan),
                    user_home.as_deref(),
                    cwd.path(),
                    program,
                    args,
                );
                (bwrap.clone(), a)
            }
            Some(Backend::Off(_)) => (program, args),
            None => match standalone_pure_compute_backend() {
                #[cfg(target_os = "macos")]
                Some(StandalonePureBackend::Seatbelt) => {
                    let profile =
                        profile::pure_compute_seatbelt_with_rules(None, user_home.as_deref());
                    let mut a = vec!["-p".into(), profile.into(), program.into()];
                    a.extend(args);
                    (PathBuf::from(SANDBOX_EXEC), a)
                }
                #[cfg(target_os = "linux")]
                Some(StandalonePureBackend::Bwrap(bwrap)) => {
                    let a = profile::pure_compute_bwrap_args(
                        None,
                        user_home.as_deref(),
                        cwd.path(),
                        program,
                        args,
                    );
                    (bwrap.clone(), a)
                }
                None => (program, args),
            },
        };

        let mut command = tokio::process::Command::new(wrapped_program);
        command
            .args(wrapped_args)
            .env_clear()
            .current_dir(cwd.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);

        // SAFETY: `PureComputeLimits::apply_self` calls only async-signal-safe `getrlimit` and
        // `setrlimit` on stack POD structs (zero heap allocation, zero locks).
        unsafe {
            command.pre_exec(move || {
                limits.apply_self();
                Ok(())
            });
        }

        let child = command.spawn()?;
        Ok(CapabilityProcess { child, _cwd: cwd })
    }

    /// Starts a local capability server with private temporary cwd, piped MCP stdio and this
    /// sandbox (including the fresh level-2 environment). Dropping it stops its process group and
    /// removes the scratch directory. Refuses to spawn when no OS sandbox backend is active.
    pub fn spawn_capability(
        sandbox: &Self,
        program: PathBuf,
        args: Vec<std::ffi::OsString>,
        env: Vec<(String, String)>,
    ) -> std::io::Result<CapabilityProcess> {
        if let Err(why) = sandbox.state() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("capability process requires an active OS sandbox: {why}"),
            ));
        }
        let cwd = tempfile::Builder::new().prefix("steve-mcp-").tempdir()?;
        let (program, args) = sandbox.0.wrap_in(program, args, &[], &[], Some(cwd.path()));
        let mut command = tokio::process::Command::new(program);
        if let Some(fresh) = sandbox.0.fresh_env() {
            command.env_clear().envs(fresh.iter().cloned());
        }
        let child = command
            .args(args)
            .current_dir(cwd.path())
            .envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        Ok(CapabilityProcess { child, _cwd: cwd })
    }
}

/// A capability process owned by the Hub, started through the shared sandbox face.
pub struct CapabilityProcess {
    pub child: tokio::process::Child,
    _cwd: tempfile::TempDir,
}

impl Drop for CapabilityProcess {
    fn drop(&mut self) {
        // The child has not been waited/reaped, so its id cannot have been reused. Stop wrapper
        // descendants too (npx and similar launchers), not just the immediate child.
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell reports readiness from inside its sandbox, then reads only after the parent
    /// creates the file. Timeouts bound broken I/O; neither side guesses when the other ran.
    async fn read_late_file(
        confine: &Confine,
        file: &Path,
        cwd: Option<&Path>,
    ) -> std::process::Output {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::time::{Duration, timeout};

        let (program, args) = confine.wrap_in(
            "/bin/sh".into(),
            vec![
                "-c".into(),
                "printf 'ready\\n'; IFS= read -r signal || exit 97; exec cat \"$1\"".into(),
                "late-file-probe".into(),
                file.as_os_str().to_owned(),
            ],
            &[],
            &[],
            cwd,
        );
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut ready = [0; 6];
        timeout(
            Duration::from_secs(5),
            child.stdout.as_mut().unwrap().read_exact(&mut ready),
        )
        .await
        .expect("sandbox readiness")
        .unwrap();
        assert_eq!(&ready, b"ready\n");
        std::fs::write(file, "late").unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"go\n")
            .await
            .unwrap();
        timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .expect("late-file read completed")
            .unwrap()
    }

    #[tokio::test]
    async fn the_late_file_barrier_allows_a_read_without_confinement() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("late");
        let confine = Confine::choose(t.path(), t.path(), Some("off".into()));
        let output = read_late_file(&confine, &file, None).await;
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"late");
    }

    #[test]
    fn off_when_asked_or_unknown() {
        let h = Path::new("/nonexistent-steve-home");
        assert!(!Confine::choose(h, h, Some("off".into())).on());
        let c = Confine::choose(h, h, Some("maybe".into()));
        assert!(
            matches!(&c.sandbox, Backend::Off(why) if why.contains("maybe")),
            "{c:?}"
        );
        let (p, a) = c.wrap("/bin/echo".into(), vec!["x".into()], &[]);
        assert_eq!(
            (p, a),
            (PathBuf::from("/bin/echo"), vec![OsString::from("x")])
        );
    }

    #[test]
    fn bwrap_masks_the_home_then_binds_the_run_dirs_back() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join("signing")).unwrap();
        std::fs::write(home.join("steve.env"), "").unwrap();
        let c = Confine {
            sandbox: Backend::Bwrap {
                bwrap: "/usr/bin/bwrap".into(),
                steve_home: home.clone(),
                node_home: home.join("data/node"),
            },
            files: false,
            env: None,
        };
        let real = std::fs::canonicalize(&home).unwrap();
        let (p, a) = c.wrap_in(
            "/bin/sh".into(),
            vec!["-c".into(), "x".into()],
            &[],
            &[],
            Some(&home.join("worktrees")),
        );
        assert_eq!(p, PathBuf::from("/usr/bin/bwrap"));
        let a: Vec<String> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        let at = |s: &str| a.iter().position(|x| x == s).unwrap();
        let r = |rel: &str| real.join(rel).to_string_lossy().into_owned();
        let home_s = real.to_string_lossy().into_owned();
        assert_eq!(a[at(&home_s) - 1], "--tmpfs", "the whole home masked first");
        assert_eq!(
            a[at("--chdir") + 1],
            r("worktrees"),
            "into the run's own directory"
        );
        // the run directories exist and are bound back writable, then the Node's secrets hidden
        assert_eq!(a[at(&r("worktrees")) - 1], "--bind");
        assert_eq!(a[at(&r("scratch")) - 1], "--bind");
        assert_eq!(a[at(&r("data/node")) - 1], "--bind");
        assert_eq!(a[at(&r("data/node/secrets")) - 1], "--tmpfs");
        assert!(real.join("data/node/secrets").is_dir());
        assert!(
            at(&home_s) < at(&r("data/node")) && at(&r("data/node")) < at(&r("data/node/secrets"))
        );
        // nothing else of the home is named: the mask hides it
        assert!(
            !a.iter()
                .any(|x| x.ends_with("signing") || x.ends_with("steve.env"))
        );
        assert_eq!(&a[a.len() - 4..], ["--", "/bin/sh", "-c", "x"]);
    }

    /// On this Mac: a run cannot read the Node's secrets but reads what it was given.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_wrapped_command_cannot_read_the_control_plane() {
        if !seatbelt_works() {
            eprintln!("skipped: no seatbelt here (inside a sandbox already?)");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let home = t.path().canonicalize().unwrap().join("home");
        let node = home.join("data/node");
        std::fs::create_dir_all(node.join("secrets/op1")).unwrap();
        std::fs::create_dir_all(home.join("signing")).unwrap();
        std::fs::write(home.join("signing/k"), "k").unwrap();
        std::fs::write(node.join("secrets/op1/V"), "v").unwrap();
        let c = Confine::choose(&home, &node, None);
        assert!(c.on(), "{c:?}");
        let run = |cmd: String, readable: &[PathBuf]| {
            let (p, a) = c.wrap("/bin/sh".into(), vec!["-c".into(), cmd.into()], readable);
            std::process::Command::new(p)
                .args(a)
                .status()
                .unwrap()
                .success()
        };
        assert!(!run(format!("cat {}/signing/k", home.display()), &[]));
        assert!(!run(format!("cat {}/secrets/op1/V", node.display()), &[]));
        let given = [node.join("secrets/op1")];
        assert!(run(format!("cat {}/secrets/op1/V", node.display()), &given));
        assert!(run("true".into(), &[]));
        let output = read_late_file(&c, &home.join("steve.env.new"), None).await;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }

    /// On a Linux machine with bubblewrap: the Hub's home is hidden whole, files the Hub writes
    /// there after the run started included, the run directories stay writable, a command's own
    /// secrets stay readable, and `/proc/<parent>/root` is no way around it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_wrapped_command_cannot_reach_the_control_plane_on_linux() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().canonicalize().unwrap().join("home");
        let node = home.join("data/node");
        for d in ["signing", "worktrees/ST-1", "data/node/secrets/op1"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::write(home.join("signing/k"), "k").unwrap();
        std::fs::write(home.join("steve.env"), "STEVE_X=1").unwrap();
        std::fs::write(node.join("node.sqlite3"), "j").unwrap();
        std::fs::write(node.join("secrets/op1/V"), "v").unwrap();
        let c = Confine::choose(&home, &node, Some("on".into()));
        if !c.on() {
            eprintln!("skipped: {c:?}");
            return;
        }
        let h = home.display();
        let run = |cmd: String, readable: &[PathBuf]| {
            let (p, a) = c.wrap_in(
                "/bin/sh".into(),
                vec!["-c".into(), cmd.into()],
                readable,
                &[],
                Some(&home.join("worktrees/ST-1")),
            );
            std::process::Command::new(p).args(a).output().unwrap()
        };
        let ok = |cmd: String, readable: &[PathBuf]| {
            let o = run(cmd.clone(), readable);
            assert!(o.status.success(), "{cmd}: {o:?}");
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        let no = |cmd: String| {
            let o = run(cmd.clone(), &[]);
            assert!(!o.status.success(), "allowed: {cmd}: {o:?}");
        };
        no(format!("cat {h}/signing/k"));
        no(format!("cat {h}/steve.env"));
        no(format!("cat {}/node.sqlite3", node.display()));
        no(format!("cat {}/secrets/op1/V", node.display()));
        no(format!("cat /proc/$PPID/root{h}/signing/k"));
        // its own directory, the run's cwd
        assert_eq!(
            ok("echo hi > f && cat f && pwd".into(), &[]),
            format!("hi\n{h}/worktrees/ST-1\n")
        );
        assert_eq!(
            ok(
                format!("cat {}/secrets/op1/V", node.display()),
                &[node.join("secrets/op1")]
            ),
            "v"
        );
        // a file the Hub writes after the run started is hidden too
        let output = read_late_file(
            &c,
            &home.join("steve.env.new"),
            Some(&home.join("worktrees/ST-1")),
        )
        .await;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }

    /// Level 1 on this Mac: a run writes in its own places and nowhere else; reads stay open.
    #[cfg(target_os = "macos")]
    #[test]
    fn level_one_keeps_writes_to_the_runs_places() {
        if !seatbelt_works() {
            eprintln!("skipped: no seatbelt here (inside a sandbox already?)");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let (ws, other) = (root.join("ws"), root.join("other"));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("r"), "readable").unwrap();
        // only the workspace here (the real list opens the temporary directories too)
        let profile = format!(
            "(version 1)\n(allow default)\n{}",
            seatbelt_writes(&Places {
                dirs: vec![ws.clone()],
                files: vec![]
            })
        );
        let sh = |cmd: String| {
            std::process::Command::new(SANDBOX_EXEC)
                .args(["-p", &profile, "/bin/sh", "-c", &cmd])
                .output()
                .unwrap()
                .status
                .success()
        };
        assert!(sh(format!("echo x > {}/f", ws.display())));
        assert!(!sh(format!("echo x > {}/f", other.display())));
        assert!(sh(format!("cat {}/r > /dev/null", other.display())));
        assert!(!other.join("f").exists());
    }

    /// A worktree's commits go into its repository's `.git`, which level 1 lets it write.
    #[test]
    fn a_worktree_finds_its_repository() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let git = |dir: &Path, args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir)
                    .env("GIT_AUTHOR_NAME", "t")
                    .env("GIT_AUTHOR_EMAIL", "t@t")
                    .env("GIT_COMMITTER_NAME", "t")
                    .env("GIT_COMMITTER_EMAIL", "t@t")
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&repo, &["worktree", "add", "-q", "../wt"]);
        assert_eq!(git_common_dir(&root.join("wt")), Some(repo.join(".git")));
        assert_eq!(
            git_common_dir(&repo),
            None,
            "a repository itself needs nothing more"
        );
    }

    /// Level 2: runs start from what a login shell gives the user, not from the Node's process.
    #[test]
    fn level_two_is_a_fresh_login_environment() {
        // SAFETY: a variable only this test sets and reads
        unsafe { std::env::set_var("STEVE_CONFINE_TEST_ONLY_HERE", "1") };
        let env = login_env();
        let has = |k: &str| env.iter().any(|(e, _)| e == k);
        assert!(has("HOME") && has("PATH") && has("LANG"), "{env:?}");
        assert!(!has("STEVE_CONFINE_TEST_ONLY_HERE"), "inherited: {env:?}");
        let h = Path::new("/nonexistent-steve-home");
        let c = Confine::choose(h, h, Some("2".into()));
        assert_eq!(c.fresh_env().is_some(), c.on(), "{c:?}");
        let c = Confine::choose(h, h, Some("1".into()));
        assert!(c.fresh_env().is_none());
        assert!(!Confine::choose(h, h, Some("0".into())).on());
    }

    #[test]
    fn isolated_runs_write_no_harness_home_of_the_owner() {
        let t = tempfile::tempdir().unwrap();
        let fake_home = t.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let ws = t.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let isolated = writable_places(&ws, &[], true, Some(&fake_home));
        assert!(!isolated.dirs.contains(&fake_home.join(".claude")));
        assert!(!isolated.dirs.contains(&fake_home.join(".config")));
        assert!(!isolated.files.contains(&fake_home.join(".claude.json")));

        let ambient = writable_places(&ws, &[], false, Some(&fake_home));
        assert!(ambient.dirs.contains(&fake_home.join(".claude")));
        assert!(ambient.dirs.contains(&fake_home.join(".config/goose")));
        assert!(ambient.dirs.contains(&fake_home.join(".config")));
        assert!(ambient.files.contains(&fake_home.join(".claude.json")));

        let creds = cli_credential_paths(&fake_home);
        assert!(creds.contains(&fake_home.join(".claude.json")));
        assert!(creds.contains(&fake_home.join(".qoder.json")));
        assert!(creds.contains(&fake_home.join(".local/share/opencode")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn non_owner_seatbelt_denies_ambient_cli_credentials() {
        if !seatbelt_works() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let fake_home = root.join("u");
        let ws = root.join("ws");
        let principal_state = root.join("node/state/p1/harness");
        std::fs::create_dir_all(fake_home.join(".local/share/opencode")).unwrap();
        std::fs::write(fake_home.join(".local/share/opencode/auth.json"), "secret").unwrap();
        std::fs::write(fake_home.join(".claude.json"), "{\"oauth\":\"tok\"}").unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&principal_state).unwrap();
        std::fs::write(principal_state.join("ok.txt"), "mine").unwrap();

        let c = Confine::choose(&root.join("steve"), &root.join("node"), Some("1".into()));
        let run = |cmd: String, hide: bool| {
            let (p, a) = c.wrap_in_mode(
                "/bin/sh".into(),
                vec!["-c".into(), cmd.into()],
                &[],
                std::slice::from_ref(&principal_state),
                Some(&ws),
                true,
                hide,
                Some(&fake_home),
                None,
            );
            std::process::Command::new(p)
                .args(a)
                .output()
                .unwrap()
                .status
                .success()
        };
        assert!(!run(
            format!(
                "cat {}/.local/share/opencode/auth.json",
                fake_home.display()
            ),
            true
        ));
        assert!(!run(
            format!("cat {}/.claude.json", fake_home.display()),
            true
        ));
        assert!(run(
            format!("cat {}/ok.txt", principal_state.display()),
            true
        ));
    }

    #[test]
    fn the_nodes_sign_in_store_opens_its_names_only() {
        let t = tempfile::tempdir().unwrap();
        let store = t.path().canonicalize().unwrap().join(".claude");
        std::fs::create_dir_all(store.join("skills")).unwrap();
        std::fs::create_dir_all(store.join(".storage-write.lock")).unwrap();
        std::fs::write(store.join(".credentials.json"), "{}").unwrap();
        std::fs::write(store.join("CLAUDE.md"), "own").unwrap();
        let l = LoginStore {
            dir: store.clone(),
            names: vec![
                ".credentials.json".into(),
                ".storage-write.lock".into(),
                ".credentials.json.tmp.*".into(),
            ],
        };
        // Bubblewrap: the directory bound, everything else in it hidden again
        let mut a = vec![];
        bwrap_login(&mut a, &l);
        let a: Vec<String> = a.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(a[..3], ["--bind".to_string(), s(&store), s(&store)]);
        let at = |path: String| a.iter().position(|x| *x == path).map(|i| a[i - 1].clone());
        assert_eq!(at(s(&store.join("skills"))).as_deref(), Some("--tmpfs"));
        assert_eq!(
            at(s(&store.join("CLAUDE.md"))).as_deref(),
            Some("/dev/null")
        );
        assert_eq!(at(s(&store.join(".credentials.json"))), None);
        assert_eq!(at(s(&store.join(".storage-write.lock"))), None);
        // Seatbelt: those names, and what is under them, by one rule after the hiding
        let rules = seatbelt_login(&l);
        assert!(
            rules.contains(r"/(\.credentials\.json|\.storage-write\.lock|\.credentials\.json\.tmp\.[^/]*)(/.*)?$"),
            "{rules}"
        );
        assert!(
            seatbelt_login(&LoginStore {
                dir: store,
                names: vec![]
            })
            .is_empty()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn owner_seatbelt_can_lock_pis_linked_auth_but_not_write_other_files_in_the_store() {
        if !seatbelt_works() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let store = root.join("node-pi");
        let state = root.join("principal-pi");
        std::fs::create_dir(&store).unwrap();
        std::fs::create_dir(&state).unwrap();
        let auth = store.join("auth.json");
        std::fs::write(&auth, "{}").unwrap();
        let link = state.join("auth.json");
        std::os::unix::fs::symlink(&auth, &link).unwrap();
        let login = LoginStore {
            dir: store.clone(),
            names: steve_harness_spec::spec("pi", None)
                .properties
                .state
                .unwrap()
                .login
                .store_names(),
        };
        let c = Confine {
            // The tempdir normally permits writes. Simulate a store outside it.
            sandbox: Backend::Seatbelt(format!(
                "(deny file-write* (subpath \"{}\"))\n",
                store.display()
            )),
            files: true,
            env: None,
        };
        let run = |script: String, grant: bool| {
            let (p, args) = c.wrap_in_mode(
                "/bin/sh".into(),
                vec![
                    "-c".into(),
                    script.into(),
                    "pi-lock".into(),
                    link.as_os_str().into(),
                    store.as_os_str().into(),
                ],
                &[],
                std::slice::from_ref(&state),
                Some(&state),
                true,
                false,
                None,
                (grant && c.opens_principal_login_store(false)).then_some(&login),
            );
            std::process::Command::new(p)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        };
        let lock = r#"target="$(readlink "$1")" && mkdir "$target.lock" &&
            cat "$1" && rmdir "$target.lock""#;
        assert!(!run(lock.into(), false));
        assert!(run(lock.into(), true));
        assert!(!run(r#"touch "$2/unrelated""#.into(), true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn non_owner_seatbelt_may_renew_the_nodes_sign_in_but_sees_nothing_else_there() {
        if !seatbelt_works() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let fake_home = root.join("u");
        let ws = root.join("ws");
        let principal_state = root.join("node/state/p1/harness");
        let store = fake_home.join(".claude");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join(".credentials.json"), "old").unwrap();
        std::fs::write(store.join("CLAUDE.md"), "the owner's own").unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&principal_state).unwrap();
        let login = LoginStore {
            dir: store.clone(),
            names: steve_harness_spec::spec("claude-code", None)
                .properties
                .state
                .unwrap()
                .login
                .store_names(),
        };
        let c = Confine::choose(&root.join("steve"), &root.join("node"), Some("1".into()));
        let run = |cmd: String, login: Option<&LoginStore>| {
            let (p, a) = c.wrap_in_mode(
                "/bin/sh".into(),
                vec!["-c".into(), cmd.into()],
                &[],
                std::slice::from_ref(&principal_state),
                Some(&ws),
                true,
                true,
                Some(&fake_home),
                login,
            );
            std::process::Command::new(p)
                .args(a)
                .output()
                .unwrap()
                .status
                .success()
        };
        let d = store.display();
        // renewing as Claude Code does: its lock, a temporary file renamed over the credentials
        let renew = format!(
            "mkdir {d}/.storage-write.lock && printf new > {d}/.credentials.json.tmp.0a1b2c3d \
             && mv {d}/.credentials.json.tmp.0a1b2c3d {d}/.credentials.json \
             && rmdir {d}/.storage-write.lock"
        );
        assert!(!run(renew.clone(), None), "hidden without the store");
        assert!(run(renew, Some(&login)));
        assert_eq!(
            std::fs::read_to_string(store.join(".credentials.json")).unwrap(),
            "new"
        );
        assert!(!run(format!("cat {d}/CLAUDE.md"), Some(&login)));
        assert!(!run(format!("printf x > {d}/other"), Some(&login)));
    }
}

#[cfg(test)]
#[test]
fn pre_extraction_node_profiles_are_byte_identical() {
    // Captured by nextest from the old implementations at c8af7504. Only fixture
    // path parameters become $STEVE_HOME / $ROOT; all policy bytes and argument order are kept.
    let c = Confine {
        sandbox: Backend::Seatbelt(
            seatbelt_rules(Path::new("/fixture/steve"), Path::new("/fixture/node")).unwrap(),
        ),
        files: false,
        env: None,
    };
    let login = LoginStore {
        dir: "/fixture/user/login".into(),
        names: vec!["auth.json".into(), "auth.json.tmp*".into()],
    };
    let (_, args) = c.wrap_in_mode(
        "/bin/tool".into(),
        vec!["hello".into()],
        &["/fixture/node/secrets/one".into()],
        &["/fixture/node/state/one".into()],
        Some(Path::new("/fixture/work")),
        true,
        true,
        Some(Path::new("/fixture/user")),
        Some(&login),
    );
    assert_eq!(
        args[1].as_encoded_bytes(),
        include_bytes!("../tests/fixtures/node.sbpl")
    );
    let writes = seatbelt_writes(&Places {
        dirs: vec!["/fixture/work".into(), "/fixture/harness".into()],
        files: vec!["/fixture/user/preferences".into()],
    });
    assert_eq!(
        writes.as_bytes(),
        include_bytes!("../tests/fixtures/writes.sbpl")
    );
    #[cfg(target_os = "macos")]
    {
        let places = writable_places_with(
            Path::new("/fixture/work"),
            &["/fixture/node/state/one".into()],
            true,
            Some(Path::new("/fixture/user")),
            &|key| (key == "TMPDIR").then(|| "/fixture/tmp".into()),
        );
        let profile = profile::seatbelt_run(
            c.seatbelt_rules().unwrap(),
            Some(&places),
            &cli_credential_paths(Path::new("/fixture/user")),
            &["/fixture/node/secrets/one".into()],
            &["/fixture/node/state/one".into()],
            Some(&login),
        );
        assert_eq!(
            profile.as_bytes(),
            include_bytes!("../tests/fixtures/node-level-one-macos.sbpl")
        );
    }
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let home = root.join("home");
    let node = home.join("data/node");
    let user = root.join("user");
    for path in [
        home.join("signing"),
        home.join("worktrees"),
        home.join("scratch"),
        node.join("secrets/one"),
        node.join("state/one"),
        user.join(".ssh"),
        user.join("login"),
        root.join("work"),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(node.join("node.sqlite3"), "").unwrap();
    std::fs::write(user.join("login/auth.json"), "").unwrap();
    std::fs::write(user.join("login/other.json"), "").unwrap();
    let c = Confine {
        sandbox: Backend::Bwrap {
            bwrap: "/usr/bin/bwrap".into(),
            steve_home: home,
            node_home: node.clone(),
        },
        files: false,
        env: None,
    };
    let login = LoginStore {
        dir: user.join("login"),
        names: vec!["auth.json".into()],
    };
    let plans = [
        c.wrap_in_mode(
            "/bin/tool".into(),
            vec!["hello".into()],
            &[node.join("secrets/one")],
            &[node.join("state/one")],
            Some(&root.join("work")),
            true,
            true,
            Some(&user),
            Some(&login),
        ),
        c.wrap_readonly(
            "/bin/tool".into(),
            vec!["hello".into()],
            &[node.join("state/one")],
            Some(&root.join("work")),
            &user,
        ),
        bwrap_readonly_standalone(
            Path::new("/usr/bin/bwrap"),
            "/bin/tool".into(),
            vec!["hello".into()],
            &[node.join("state/one")],
            Some(&root.join("work")),
            &user,
        ),
    ];
    let plans: Vec<_> = plans
        .into_iter()
        .map(|(p, a)| {
            let normalize = |s: &std::ffi::OsStr| {
                s.to_string_lossy()
                    .replace(root.join("home").to_str().unwrap(), "$STEVE_HOME")
                    .replace(root.to_str().unwrap(), "$ROOT")
            };
            (
                normalize(p.as_os_str()),
                a.iter().map(|s| normalize(s)).collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        serde_json::to_value(plans).unwrap(),
        serde_json::from_str::<serde_json::Value>(include_str!("../tests/fixtures/bwrap.json"))
            .unwrap()
    );
}

#[cfg(test)]
#[test]
fn pre_extraction_research_and_control_profiles_are_byte_identical() {
    // These are old Research/Vault outputs, never snapshots recorded by this renderer.
    assert_eq!(
        readonly_profile(&["/fixture/run".into(), "/fixture/user/state".into()])
            .unwrap()
            .as_bytes(),
        include_bytes!("../tests/fixtures/research.sbpl")
    );
    assert_eq!(
        seatbelt_rules(Path::new("/fixture/steve"), Path::new("/fixture/node"))
            .unwrap()
            .as_bytes(),
        include_bytes!("../tests/fixtures/control.sbpl")
    );
}

// This fixture uses the Linux backend even on macOS; no bwrap process is executed.
#[cfg(test)]
fn level_one_bwrap_fixture() -> serde_json::Value {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let home = root.join("steve");
    let node = home.join("data/node");
    let user = root.join("user");
    let work = root.join("work");
    for path in [
        home.join("signing"),
        home.join("worktrees"),
        home.join("scratch"),
        node.join("secrets/one"),
        node.join("state/one"),
        user.join(".cache"),
        user.join(".npm"),
        user.join(".claude"),
        user.join(".ssh"),
        work.clone(),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(node.join("node.sqlite3"), "").unwrap();
    std::fs::write(user.join(".claude.json"), "").unwrap();
    let c = Confine {
        sandbox: Backend::Bwrap {
            bwrap: "/usr/bin/bwrap".into(),
            steve_home: home,
            node_home: node.clone(),
        },
        files: true,
        env: None,
    };
    let (program, args) = c.wrap_in_mode(
        "/bin/tool".into(),
        vec!["hello".into()],
        &[node.join("secrets/one")],
        &[node.join("state/one")],
        Some(&work),
        false,
        true,
        Some(&user),
        None,
    );
    let normalize =
        |p: &std::ffi::OsStr| p.to_string_lossy().replace(root.to_str().unwrap(), "$ROOT");
    serde_json::to_value((
        normalize(program.as_os_str()),
        args.iter().map(|a| normalize(a)).collect::<Vec<_>>(),
    ))
    .unwrap()
}

#[cfg(test)]
#[test]
fn pre_extraction_level_one_bwrap_is_byte_identical() {
    // Captured from Confine { files: true } at c8af7504, never from this renderer.
    let actual = level_one_bwrap_fixture();
    let (program, args): (String, Vec<String>) =
        serde_json::from_str(include_str!("../tests/fixtures/bwrap-level-one.json")).unwrap();
    let tmp = std::env::var_os("TMPDIR");
    let mut expected = Vec::new();
    let mut at = 0;
    while at < args.len() {
        // Legacy Places probes these four system directories and optional TMPDIR. Their
        // presence is an input: macOS captures must also run on Linux without inventing mounts.
        if args[at] == "--bind" && at + 2 < args.len() && args[at + 1] == args[at + 2] {
            let path = &args[at + 1];
            if ["/tmp", "/private/tmp", "/var/tmp", "/private/var/folders"].contains(&path.as_str())
                && !Path::new(path).is_dir()
                || path == "$TMPDIR" && tmp.is_none()
            {
                at += 3;
                continue;
            }
        }
        expected.push(if args[at] == "$TMPDIR" {
            tmp.as_ref().unwrap().to_string_lossy().into_owned()
        } else {
            args[at].clone()
        });
        at += 1;
    }
    assert_eq!(actual, serde_json::to_value((program, expected)).unwrap());
}

#[cfg(test)]
#[tokio::test]
async fn review_08_pure_compute_confinement_enforces_ro_nonet_empty_cwd_and_rlimits() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let steve_home = root.join("steve");
    let node_home = steve_home.join("data/node");
    let user_home = root.join("user");
    let empty_cwd = root.join("empty-cwd");
    std::fs::create_dir_all(steve_home.join("signing")).unwrap();
    std::fs::create_dir_all(node_home.join("secrets")).unwrap();
    std::fs::create_dir_all(user_home.join(".steve")).unwrap();
    std::fs::create_dir_all(user_home.join(".ssh")).unwrap();
    std::fs::create_dir_all(&empty_cwd).unwrap();
    std::fs::write(steve_home.join("signing/key"), "secret").unwrap();

    // 1. Seatbelt policy must deny network, deny writes (except /dev/null), deny fork, and hide Steve/user secrets.
    let sb = pure_compute_seatbelt_profile(Some((&steve_home, &node_home)), Some(&user_home))
        .expect("pure-compute seatbelt profile");
    assert!(sb.contains("(deny network*)"), "{sb}");
    assert!(sb.contains("(deny file-write*)"), "{sb}");
    assert!(sb.contains("(deny process-fork)"), "{sb}");
    assert!(sb.contains(&steve_home.display().to_string()), "{sb}");
    assert!(
        sb.contains(&user_home.join(".steve").display().to_string()),
        "{sb}"
    );
    assert!(
        sb.contains(&user_home.join(".ssh").display().to_string()),
        "{sb}"
    );

    // 2. Bubblewrap policy must unshare network, mount root read-only, mask Steve/user secrets, and chdir into empty temp dir.
    let plan = bwrap_plan(&steve_home, &node_home);
    let bwrap_args = pure_compute_bwrap_args(
        Some(&plan),
        Some(&user_home),
        &empty_cwd,
        PathBuf::from("/usr/bin/steve-hub"),
        vec!["--workflow-step".into()],
    );
    let args: Vec<String> = bwrap_args
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    assert!(args.contains(&"--unshare-net".to_owned()), "{args:?}");
    assert!(args.contains(&"--die-with-parent".to_owned()), "{args:?}");
    assert!(
        args.windows(3).any(|w| w == ["--ro-bind", "/", "/"]),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--chdir" && w[1] == empty_cwd.to_string_lossy()),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--tmpfs" && w[1] == steve_home.to_string_lossy()),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--tmpfs" && w[1] == user_home.join(".steve").to_string_lossy()),
        "{args:?}"
    );

    // 3. Real spawned process gets cleared env, empty temp cwd, and OS rlimits (CPU/FSIZE/CORE).
    let limits = PureComputeLimits::for_wall_time(std::time::Duration::from_secs(5));
    let mut proc = Sandbox::spawn_pure_compute(
        None,
        PathBuf::from("/bin/sh"),
        vec![
            "-c".into(),
            "pwd; ulimit -t; ulimit -f; ulimit -c; echo HOME=${HOME:-empty}".into(),
        ],
        limits,
    )
    .unwrap();
    let mut out_bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut proc.child.stdout.take().unwrap(), &mut out_bytes)
        .await
        .unwrap();
    let status = proc.child.wait().await.unwrap();
    assert!(status.success(), "probe failed: {status:?}");
    let stdout = String::from_utf8_lossy(&out_bytes);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 5, "unexpected output:\n{stdout}");
    assert!(
        lines[0].contains("steve-wf-step-"),
        "expected empty temp cwd, got: {}",
        lines[0]
    );
    assert_eq!(
        lines[1].trim(),
        "7",
        "expected RLIMIT_CPU=5+2s, got: {}",
        lines[1]
    );
    assert_eq!(
        lines[2].trim(),
        "0",
        "expected RLIMIT_FSIZE=0, got: {}",
        lines[2]
    );
    assert_eq!(
        lines[3].trim(),
        "0",
        "expected RLIMIT_CORE=0, got: {}",
        lines[3]
    );
    assert_eq!(
        lines[4].trim(),
        "HOME=empty",
        "expected env_clear, got: {}",
        lines[4]
    );

    // 4. Attempting to write a file is killed by SIGXFSZ (RLIMIT_FSIZE=0) or denied by RO sandbox.
    let mut write_proc = Sandbox::spawn_pure_compute(
        None,
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), "echo leak > leak.txt".into()],
        limits,
    )
    .unwrap();
    let write_status = write_proc.child.wait().await.unwrap();
    assert!(
        !write_status.success(),
        "writing a file inside pure-compute sandbox must fail, got {write_status:?}"
    );
}
