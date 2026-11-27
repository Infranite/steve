//! macOS `posix_spawn` wrapper shared by detached-session execution (`DetachedCommand`) and the
//! backup engine (inherited lock fd). Avoids calling `fork()` in multithreaded macOS processes
//! where CoreFoundation / Security / libdispatch locks can deadlock child threads, while matching
//! `std::process::Command` signal semantics (`POSIX_SPAWN_SETSIGDEF` for `SIGPIPE` and empty
//! `POSIX_SPAWN_SETSIGMASK`).

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use libc::{
    POSIX_SPAWN_CLOEXEC_DEFAULT, POSIX_SPAWN_SETPGROUP, POSIX_SPAWN_SETSIGDEF,
    POSIX_SPAWN_SETSIGMASK, posix_spawn as sys_spawn,
    posix_spawn_file_actions_addchdir_np as fa_addchdir,
    posix_spawn_file_actions_adddup2 as fa_adddup2, posix_spawn_file_actions_addopen as fa_addopen,
    posix_spawn_file_actions_destroy as fa_destroy, posix_spawn_file_actions_init as fa_init,
    posix_spawn_file_actions_t as Fa, posix_spawnattr_destroy as attr_destroy,
    posix_spawnattr_init as attr_init, posix_spawnattr_setflags as attr_setflags,
    posix_spawnattr_setpgroup as attr_setpgroup, posix_spawnattr_setsigdefault as attr_setsigdef,
    posix_spawnattr_setsigmask as attr_setsigmask, posix_spawnattr_t as Attr,
};

use super::ProcessIsolation;

/// macOS 10.14+ `<sys/spawn.h>` flag that starts the child in a new session (`setsid`) directly
/// inside the kernel without invoking `fork()` + `pre_exec`.
const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;

/// First file descriptor slot above stdin (0), stdout (1), and stderr (2).
const FIRST_NON_STDIO_FD: RawFd = 3;

/// Standard I/O wiring for [`spawn`].
#[derive(Clone, Copy, Debug)]
pub enum StdioSpec {
    /// Redirect the stream to `/dev/null`.
    Null,
    /// Create a fresh `FD_CLOEXEC` pipe and return the parent endpoint in [`Spawned`].
    Pipe,
    /// Duplicate this caller-owned open descriptor onto the child's stdio slot.
    Fd(RawFd),
}

/// Environment wiring for [`spawn`].
pub enum EnvSpec<'a> {
    /// Clear the environment and pass only these exact `KEY=VALUE` entries.
    Clear(&'a [(OsString, OsString)]),
    /// Inherit the parent process's environment and apply explicit overrides (`Some`) / removals
    /// (`None`) in order.
    InheritWithDeltas(&'a [(OsString, Option<OsString>)]),
}

/// Parameters for [`spawn`].
pub struct SpawnSpec<'a> {
    pub program: &'a OsStr,
    pub args: &'a [&'a OsStr],
    pub env: EnvSpec<'a>,
    pub cwd: Option<&'a Path>,
    pub isolation: ProcessIsolation,
    pub stdin: StdioSpec,
    pub stdout: StdioSpec,
    pub stderr: StdioSpec,
    /// Additional caller-owned descriptors to inherit across `exec` (at distinct slots `>= 3`
    /// with `FD_CLOEXEC` cleared in the child, e.g. an exclusive repository lock file).
    pub extra_inherit_fds: &'a [RawFd],
}

/// Result of [`spawn`]: direct child PID and parent-side pipe descriptors (if requested).
pub struct Spawned {
    pub pid: libc::pid_t,
    pub stdin: Option<OwnedFd>,
    pub stdout: Option<OwnedFd>,
    pub stderr: Option<OwnedFd>,
}

struct FileActions(Fa);
impl FileActions {
    fn new() -> io::Result<Self> {
        let mut raw = std::mem::MaybeUninit::<Fa>::uninit();
        // SAFETY: initializes an opaque POSIX file-actions handle.
        let rc = unsafe { fa_init(raw.as_mut_ptr()) };
        check_rc(rc)?;
        // SAFETY: initialized by `fa_init`.
        Ok(Self(unsafe { raw.assume_init() }))
    }
}
impl Drop for FileActions {
    fn drop(&mut self) {
        // SAFETY: `self.0` was initialized by `fa_init`.
        unsafe {
            fa_destroy(&mut self.0);
        }
    }
}

struct SpawnAttr(Attr);
impl SpawnAttr {
    fn new() -> io::Result<Self> {
        let mut raw = std::mem::MaybeUninit::<Attr>::uninit();
        // SAFETY: initializes an opaque POSIX spawnattr handle.
        let rc = unsafe { attr_init(raw.as_mut_ptr()) };
        check_rc(rc)?;
        // SAFETY: initialized by `attr_init`.
        Ok(Self(unsafe { raw.assume_init() }))
    }
}
impl Drop for SpawnAttr {
    fn drop(&mut self) {
        // SAFETY: `self.0` was initialized by `attr_init`.
        unsafe {
            attr_destroy(&mut self.0);
        }
    }
}

fn check_rc(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

fn cstring_os(s: &OsStr) -> io::Result<CString> {
    CString::new(s.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "subprocess argument or environment entry contains interior NUL",
        )
    })
}

fn cloexec_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: valid 2-element array for pipe(2).
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `pipe` succeeded and returned two owned descriptors.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    for fd in [read.as_raw_fd(), write.as_raw_fd()] {
        // SAFETY: `fd` is owned and valid.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((read, write))
}

fn dup_cloexec_above_stdio(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `F_DUPFD_CLOEXEC` duplicates an open descriptor to a fresh slot >= 3 with CLOEXEC.
    let duped = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, FIRST_NON_STDIO_FD) };
    if duped < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `duped` is a newly created owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duped) })
}

/// Spawn a child process directly through macOS `posix_spawn` without `fork()`.
///
/// `spec.program` is resolved into an absolute executable path strictly against the child's own
/// effective `PATH` (from `spec.env`) and optional `spec.cwd`. It never falls back to
/// `posix_spawnp` or the parent process's ambient `PATH` when the child cleared or customized
/// `PATH`.
pub fn spawn(spec: SpawnSpec<'_>) -> io::Result<Spawned> {
    let env_entries: Vec<(OsString, OsString)> = match spec.env {
        EnvSpec::Clear(entries) => entries.to_vec(),
        EnvSpec::InheritWithDeltas(deltas) => {
            let mut merged: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
            for (k, v) in deltas {
                match v {
                    Some(val) => {
                        merged.insert(k.clone(), val.clone());
                    }
                    None => {
                        merged.remove(k);
                    }
                }
            }
            merged.into_iter().collect()
        }
    };

    // Resolve `spec.program` strictly against the child's effective `PATH` before allocating pipes
    // or file actions; never fall back to `posix_spawnp` (which reads the parent's `environ`).
    let child_path = env_entries
        .iter()
        .rev()
        .find(|(k, _)| k.as_bytes() == b"PATH")
        .map(|(_, v)| v.as_os_str());
    let resolved_program = super::bounded::resolve_program(spec.program, child_path, spec.cwd)?;
    let spawn_path_c = cstring_os(resolved_program.as_os_str())?;
    let argv0_c = cstring_os(spec.program)?;

    // Keep all duplicated child-side source fds alive across `posix_spawn` at slots >= 3 so
    // `adddup2(_, 0/1/2)` never aliases another source fd.
    let mut keep_alive: Vec<OwnedFd> = Vec::new();
    let mut actions = FileActions::new()?;

    if let Some(cwd) = spec.cwd {
        let cwd_c = cstring_os(cwd.as_os_str())?;
        // SAFETY: `actions.0` is initialized; `fa_addchdir` copies the path internally.
        unsafe { check_rc(fa_addchdir(&mut actions.0, cwd_c.as_ptr()))? };
    }

    let mut parent_stdin: Option<OwnedFd> = None;
    let mut parent_stdout: Option<OwnedFd> = None;
    let mut parent_stderr: Option<OwnedFd> = None;

    let mut wire_stdio = |target_fd: RawFd,
                          stdio: StdioSpec,
                          is_stdin: bool,
                          parent_slot: &mut Option<OwnedFd>|
     -> io::Result<()> {
        match stdio {
            StdioSpec::Null => {
                let flags = if is_stdin {
                    libc::O_RDONLY
                } else {
                    libc::O_WRONLY
                };
                // SAFETY: `actions.0` is initialized and `/dev/null` is a static C string.
                unsafe {
                    check_rc(fa_addopen(
                        &mut actions.0,
                        target_fd,
                        c"/dev/null".as_ptr(),
                        flags,
                        0,
                    ))
                }
            }
            StdioSpec::Pipe => {
                let (read, write) = cloexec_pipe()?;
                let (child_end, parent_end) = if is_stdin {
                    (read, write)
                } else {
                    (write, read)
                };
                let safe_src = dup_cloexec_above_stdio(child_end.as_raw_fd())?;
                drop(child_end);
                // SAFETY: `safe_src` remains alive in `keep_alive` until after `posix_spawn`.
                unsafe { check_rc(fa_adddup2(&mut actions.0, safe_src.as_raw_fd(), target_fd))? };
                keep_alive.push(safe_src);
                *parent_slot = Some(parent_end);
                Ok(())
            }
            StdioSpec::Fd(fd) => {
                let safe_src = dup_cloexec_above_stdio(fd)?;
                // SAFETY: `safe_src` remains alive in `keep_alive` until after `posix_spawn`.
                unsafe { check_rc(fa_adddup2(&mut actions.0, safe_src.as_raw_fd(), target_fd))? };
                keep_alive.push(safe_src);
                Ok(())
            }
        }
    };

    wire_stdio(0, spec.stdin, true, &mut parent_stdin)?;
    wire_stdio(1, spec.stdout, false, &mut parent_stdout)?;
    wire_stdio(2, spec.stderr, false, &mut parent_stderr)?;

    // Duplicate extra inherited descriptors above stdio and map each to a distinct target slot
    // >= 3 so `adddup2` clears `FD_CLOEXEC` in the child even under `POSIX_SPAWN_CLOEXEC_DEFAULT`.
    let mut extra_srcs = Vec::with_capacity(spec.extra_inherit_fds.len());
    for &fd in spec.extra_inherit_fds {
        extra_srcs.push(dup_cloexec_above_stdio(fd)?);
    }
    let mut used_targets: Vec<RawFd> = Vec::with_capacity(extra_srcs.len());
    let forbidden: Vec<RawFd> = keep_alive
        .iter()
        .chain(extra_srcs.iter())
        .map(AsRawFd::as_raw_fd)
        .collect();
    for src in &extra_srcs {
        let mut target = FIRST_NON_STDIO_FD;
        while forbidden.contains(&target) || used_targets.contains(&target) {
            target += 1;
        }
        // SAFETY: `src` stays alive until after `posix_spawn` and `target != src`.
        unsafe { check_rc(fa_adddup2(&mut actions.0, src.as_raw_fd(), target))? };
        used_targets.push(target);
    }
    keep_alive.extend(extra_srcs);

    let mut attr = SpawnAttr::new()?;
    // Match Rust `std::process::Command`: reset `SIGPIPE` to `SIG_DFL` (the Rust runtime ignores
    // it) and clear any signals masked on the calling thread.
    // SAFETY: `sigemptyset` and `sigaddset` initialize stack `sigset_t` values.
    let (sigdef, sigmask) = unsafe {
        let mut sigdef: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut sigdef);
        libc::sigaddset(&mut sigdef, libc::SIGPIPE);
        let mut sigmask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut sigmask);
        (sigdef, sigmask)
    };
    let isolation_flag = match spec.isolation {
        ProcessIsolation::ProcessGroup => POSIX_SPAWN_SETPGROUP,
        ProcessIsolation::DetachedSession => POSIX_SPAWN_SETSID,
    };
    let flags = (isolation_flag
        | POSIX_SPAWN_CLOEXEC_DEFAULT
        | POSIX_SPAWN_SETSIGDEF
        | POSIX_SPAWN_SETSIGMASK) as libc::c_short;
    // SAFETY: `attr.0` is initialized and `sigdef`/`sigmask` are valid initialized signal sets.
    unsafe {
        check_rc(attr_setflags(&mut attr.0, flags))?;
        check_rc(attr_setsigdef(&mut attr.0, &sigdef))?;
        check_rc(attr_setsigmask(&mut attr.0, &sigmask))?;
        if matches!(spec.isolation, ProcessIsolation::ProcessGroup) {
            check_rc(attr_setpgroup(&mut attr.0, 0))?;
        }
    }

    let mut argv_c = Vec::with_capacity(spec.args.len() + 1);
    argv_c.push(argv0_c);
    for arg in spec.args {
        argv_c.push(cstring_os(arg)?);
    }
    let mut argv_ptrs: Vec<*mut libc::c_char> = argv_c
        .iter()
        .map(|s| s.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect();

    let mut env_c = Vec::with_capacity(env_entries.len());
    for (k, v) in &env_entries {
        let mut kv = OsString::with_capacity(k.len() + 1 + v.len());
        kv.push(k);
        kv.push("=");
        kv.push(v);
        env_c.push(cstring_os(&kv)?);
    }
    let envp_ptrs: Vec<*mut libc::c_char> = env_c
        .iter()
        .map(|s| s.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect();

    let mut pid: libc::pid_t = 0;
    // SAFETY: all pointers, file actions, attributes, and duplicated source descriptors stay
    // alive across the `posix_spawn` call.
    let rc = unsafe {
        sys_spawn(
            &mut pid,
            spawn_path_c.as_ptr(),
            &actions.0,
            &attr.0,
            argv_ptrs.as_mut_ptr(),
            envp_ptrs.as_ptr(),
        )
    };
    drop(keep_alive);
    check_rc(rc)?;
    Ok(Spawned {
        pid,
        stdin: parent_stdin,
        stdout: parent_stdout,
        stderr: parent_stderr,
    })
}
