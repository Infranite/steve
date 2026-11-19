//! The sole OS sandbox probe. Node admission supplies its full control-plane policy; generic
//! availability checks supply the historical minimal policy. Their candidate selection and
//! cache lifetimes remain unchanged.
use super::{Backend, Confine, SANDBOX_EXEC};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn starts(program: &Path, args: &[OsString]) -> Result<(), String> {
    match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "the sandbox does not start here ({}: {})",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("the sandbox does not start here ({e})")),
    }
}
fn try_backend(backend: &Backend, full: bool) -> Result<(), String> {
    let (program, args) = if full {
        Confine {
            sandbox: backend.clone(),
            files: false,
            env: None,
        }
        .wrap("/usr/bin/true".into(), vec![], &[])
    } else {
        match backend {
            Backend::Seatbelt(_) => (
                SANDBOX_EXEC.into(),
                vec![
                    "-p".into(),
                    super::profile::minimal_profile().into(),
                    "/usr/bin/true".into(),
                ],
            ),
            Backend::Bwrap { bwrap, .. } => (bwrap.clone(), super::profile::minimal_bwrap_probe()),
            Backend::Off(why) => return Err(why.clone()),
        }
    };
    starts(&program, &args)
}
pub(super) fn detect(homes: Option<(&Path, &Path)>) -> Result<Backend, String> {
    if cfg!(target_os = "macos") {
        if !Path::new(SANDBOX_EXEC).exists() {
            return Err(format!("{SANDBOX_EXEC} is missing"));
        }
        let rules = match homes {
            Some((steve, node)) => super::seatbelt_rules(steve, node)
                .ok_or_else(|| "a Steve path cannot be written in a profile".to_owned())?,
            None => String::new(),
        };
        let backend = Backend::Seatbelt(rules);
        try_backend(&backend, homes.is_some())?;
        return Ok(backend);
    }
    if cfg!(target_os = "linux") {
        let mut last = None;
        for path in ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"]
            .into_iter()
            .filter(|p| Path::new(p).exists())
        {
            let (steve, node) = homes
                .map(|(s, n)| (s.to_path_buf(), n.to_path_buf()))
                .unwrap_or_default();
            let backend = Backend::Bwrap {
                bwrap: path.into(),
                steve_home: steve,
                node_home: node,
            };
            match try_backend(&backend, homes.is_some()) {
                Ok(()) => return Ok(backend),
                // Node historically tries its first installed candidate. Generic vault checks
                // continue to the next candidate; neither path silently changes its fallback.
                Err(e) if homes.is_some() => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        return Err(last.unwrap_or_else(|| "bwrap (bubblewrap) is not installed".to_owned()));
    }
    Err("no sandbox on this platform".to_owned())
}
pub fn seatbelt_works() -> bool {
    cfg!(target_os = "macos") && detect(None).is_ok()
}
pub fn bwrap_path() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    match detect(None).ok()? {
        Backend::Bwrap { bwrap, .. } => Some(bwrap),
        _ => None,
    }
}
pub fn bwrap_works() -> bool {
    bwrap_path().is_some()
}
pub fn available() -> bool {
    static WORKS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WORKS.get_or_init(|| detect(None).is_ok())
}

#[cfg(test)]
#[test]
fn minimal_probes_preserve_the_old_argv() {
    assert_eq!(
        super::profile::minimal_profile(),
        "(version 1)(allow default)"
    );
    let args: Vec<_> = super::profile::minimal_bwrap_probe()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--",
            "/usr/bin/true"
        ]
    );
}
