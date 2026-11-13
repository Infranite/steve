//! The Node's side of a one-shot ACP call ([`steve_acp::oneshot`]): the OS sandbox its adapter
//! runs in. The caller owns the process journal and the job's deadline/cancellation policy.

use std::path::{Path, PathBuf};

use steve_acp::oneshot::{Command, Env};
pub(crate) use steve_acp::oneshot::{OneShotError, Request, run};

/// Read-only OS sandbox configuration for a one-shot ACP job (`acp_think` or `research`).
#[derive(Debug, Clone)]
pub(crate) enum ReadonlySandbox {
    /// macOS Seatbelt profile path (`-f <profile>`).
    Seatbelt(PathBuf),
    /// Linux Bubblewrap read-only filesystem with only `dir` and `writable` bound read-write.
    Bwrap {
        dir: PathBuf,
        writable: Vec<PathBuf>,
    },
}

/// How the adapter of a one-shot call is started: under `sandbox` (a read-only seatbelt profile or
/// bubblewrap mount set, with its temporary files under `<dir>/tmp`), else in the Node's sandbox
/// when the adapter nests in one (`confine`), else as it is (the harness keeps its own sandbox,
/// which macOS cannot nest in another). `cwd` is where the Node's fallback sandbox keeps the call's
/// writes (`None`: control-plane isolation without opening a workspace directory for writing).
pub(crate) fn wrap<'a>(
    sandbox: Option<&'a ReadonlySandbox>,
    confine: Option<&'a crate::confine::Confine>,
    cwd: Option<&'a Path>,
    user_home: &'a Path,
    outer_sandbox_args: &'a [String],
) -> impl Fn(Command, &mut Env) -> Command + Send + Sync + 'a {
    move |(program, mut args), env| {
        if super::confine::is_outer_sandbox_active(sandbox.is_some(), confine) {
            for flag in outer_sandbox_args {
                if !args.iter().any(|a| a == flag.as_str()) {
                    args.push(flag.into());
                }
            }
        }
        match sandbox {
            Some(ReadonlySandbox::Seatbelt(profile)) => {
                args.splice(
                    0..0,
                    [
                        "-f".into(),
                        profile.as_os_str().to_owned(),
                        program.into_os_string(),
                    ],
                );
                if let Some(parent) = profile.parent() {
                    env.push(("TMPDIR".into(), parent.join("tmp").into_os_string()));
                }
                (steve_research::sandbox::SANDBOX_EXEC.into(), args)
            }
            Some(ReadonlySandbox::Bwrap { dir, writable }) => {
                env.push(("TMPDIR".into(), dir.join("tmp").into_os_string()));
                if let Some(c) = confine {
                    c.wrap_readonly(program, args, writable, Some(dir), user_home)
                } else if let Some(bwrap) = steve_vault::harness::bwrap_path() {
                    crate::confine::bwrap_readonly_standalone(
                        &bwrap,
                        program,
                        args,
                        writable,
                        Some(dir),
                        user_home,
                    )
                } else {
                    (program, args)
                }
            }
            None => {
                if let Some(c) = confine {
                    c.wrap_in_mode(program, args, &[], &[], cwd, false, false, Some(user_home))
                } else {
                    (program, args)
                }
            }
        }
    }
}
