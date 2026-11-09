//! The Node's side of a one-shot ACP call ([`steve_acp::oneshot`]): the OS sandbox its adapter
//! runs in. The caller owns the process journal and the job's deadline/cancellation policy.

use std::path::Path;

use steve_acp::oneshot::{Command, Env};
pub(crate) use steve_acp::oneshot::{OneShotError, Request, run};

/// How the adapter of a one-shot call is started: under `profile` (a seatbelt profile of the
/// call's own, its temporary files next to it), else in the Node's sandbox when the adapter nests
/// in one (`confine`), else as it is (the harness keeps its own sandbox, which macOS cannot nest
/// in another).
pub(crate) fn wrap<'a>(
    profile: Option<&'a Path>,
    confine: Option<&'a crate::confine::Confine>,
    cwd: &'a Path,
) -> impl Fn(Command, &mut Env) -> Command + Send + Sync + 'a {
    move |(program, mut args), env| {
        if let Some(profile) = profile {
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
        } else if let Some(c) = confine {
            c.wrap_in(program, args, &[], &[], Some(cwd))
        } else {
            (program, args)
        }
    }
}
