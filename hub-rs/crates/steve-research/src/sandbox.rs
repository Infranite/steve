//! Layer 3 of read-only enforcement: on macOS the harness runs under `sandbox-exec` with a profile that
//! denies every file write except the run's scratch directory and the harness's own state (`~/.claude`,
//! where `--resume` finds the session). Even a harness that ignored its allowlist could not change a project.

use std::path::{Path, PathBuf};

/// The system tool; present on every macOS install.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

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

/// True on macOS with `sandbox-exec` installed and usable from here (not from inside another
/// sandbox, which macOS cannot nest: Steve's own tests run in a confined session). Asked once.
pub fn available() -> bool {
    static WORKS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WORKS
        .get_or_init(|| Path::new(SANDBOX_EXEC).exists() && steve_vault::harness::seatbelt_works())
}

/// Whether a run under `mode` is sandboxed; `None` when `Required` but unavailable.
pub fn decide(mode: SandboxMode) -> Option<bool> {
    match mode {
        SandboxMode::Off => Some(false),
        SandboxMode::Auto => Some(available()),
        SandboxMode::Required => available().then_some(true),
    }
}

/// The SBPL profile: everything allowed except file writes, which only reach `writable` (and `/dev/null`).
/// Paths with a quote or backslash are refused so they cannot break out of the string literal.
pub fn profile(writable: &[PathBuf]) -> Option<String> {
    let mut allow = String::new();
    for w in writable {
        let s = w.to_str()?;
        if s.contains(['"', '\\']) || !w.is_absolute() {
            return None;
        }
        allow.push_str(&format!(" (subpath \"{s}\")"));
    }
    Some(format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*{allow} (literal \"/dev/null\") (literal \"/dev/tty\"))\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_shape() {
        let p = profile(&["/s/run".into(), "/h/.claude".into()]).unwrap();
        assert!(p.contains("(deny file-write*)"));
        assert!(p.contains("(subpath \"/s/run\") (subpath \"/h/.claude\")"));
        assert!(profile(&["/a\"b".into()]).is_none());
        assert!(profile(&["rel".into()]).is_none());
    }

    /// Layer 3 on its own: a process that ignores every allowlist still cannot write outside the
    /// writable directories.
    #[cfg(target_os = "macos")]
    #[test]
    fn writes_outside_the_profile_fail() {
        if !available() {
            eprintln!("skipped: no seatbelt here (inside a sandbox already?)");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let proj = root.join("proj");
        let run = root.join("run");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(proj.join("keep.txt"), "orig").unwrap();
        let prof = profile(std::slice::from_ref(&run)).unwrap();
        let pf = run.join("p.sb");
        std::fs::write(&pf, prof).unwrap();
        let script = format!(
            "echo x > '{p}/new.txt'; echo y > '{p}/keep.txt'; rm -f '{p}/keep.txt'; echo ok > '{r}/fine.txt'",
            p = proj.display(),
            r = run.display()
        );
        let st = std::process::Command::new(SANDBOX_EXEC)
            .arg("-f")
            .arg(&pf)
            .args(["/bin/sh", "-c", &script])
            .status()
            .unwrap();
        assert!(st.success(), "last command (write in run dir) succeeds");
        assert!(!proj.join("new.txt").exists());
        assert_eq!(
            std::fs::read_to_string(proj.join("keep.txt")).unwrap(),
            "orig"
        );
        assert_eq!(
            std::fs::read_to_string(run.join("fine.txt")).unwrap(),
            "ok\n"
        );
    }
}
