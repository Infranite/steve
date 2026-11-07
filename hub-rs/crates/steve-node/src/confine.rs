//! The OS sandbox around what a Node starts for a run: a session's harness, a job's harness, a
//! command. This is the first stage of "every run in the OS sandbox" (path model S12). Steve's
//! control plane stays out of the run's reach whatever the harness does with its own shell: the
//! Hub's and the Node's secrets, keys, databases and journal cannot be read or written, and
//! Steve's own programs cannot be changed (`steve_vault::harness::{PROTECTED, NODE_PROTECTED,
//! NODE_FILES, SEALED}`). Everything else is as before; a deny-by-default profile built from the
//! run's view comes later.
//!
//! macOS uses `sandbox-exec` (seatbelt), Linux `bwrap` (bubblewrap). The Node tries the sandbox
//! once when it opens. When it is missing, fails, or `STEVE_CONFINE=off`, runs go unconfined and the
//! Node says so once at open: starting a run never fails because of this stage. A harness that runs
//! its own seatbelt sandbox (an adapter declaring `nested_sandbox: false`) is not wrapped, because
//! macOS refuses a sandbox inside a sandbox; it keeps its own.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[derive(Debug, Clone)]
pub(crate) enum Confine {
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
    pub(crate) fn open(steve_home: &Path, node_home: &Path) -> Confine {
        let c = Self::from_env(steve_home, node_home);
        match &c {
            Confine::Off(why) => tracing::warn!("node: runs are not confined: {why}"),
            Confine::Seatbelt(_) => tracing::info!("node: runs are confined (seatbelt)"),
            Confine::Bwrap { .. } => tracing::info!("node: runs are confined (bubblewrap)"),
        }
        c
    }

    fn from_env(steve_home: &Path, node_home: &Path) -> Confine {
        Self::choose(steve_home, node_home, std::env::var("STEVE_CONFINE").ok())
    }

    pub(crate) fn choose(steve_home: &Path, node_home: &Path, mode: Option<String>) -> Confine {
        match mode.as_deref().map(str::trim) {
            Some("off") => return Confine::Off("STEVE_CONFINE=off".into()),
            None | Some("") | Some("on") => {}
            Some(other) => {
                return Confine::Off(format!("STEVE_CONFINE={other} is not on or off"));
            }
        }
        let c = if cfg!(target_os = "macos") {
            if !Path::new(SANDBOX_EXEC).exists() {
                return Confine::Off(format!("{SANDBOX_EXEC} is missing"));
            }
            match steve_vault::harness::seatbelt_rules(steve_home, node_home) {
                Some(rules) => Confine::Seatbelt(rules),
                None => return Confine::Off("a Steve path cannot be written in a profile".into()),
            }
        } else if cfg!(target_os = "linux") {
            let Some(bwrap) = ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"]
                .iter()
                .map(PathBuf::from)
                .find(|p| p.exists())
            else {
                return Confine::Off("bwrap (bubblewrap) is not installed".into());
            };
            Confine::Bwrap {
                bwrap,
                steve_home: steve_home.to_path_buf(),
                node_home: node_home.to_path_buf(),
            }
        } else {
            return Confine::Off("no sandbox on this platform".into());
        };
        let (program, args) = c.wrap("/usr/bin/true".into(), vec![], &[]);
        match std::process::Command::new(&program)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(o) if o.status.success() => c,
            Ok(o) => Confine::Off(format!(
                "the sandbox does not start here ({}: {})",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Confine::Off(format!("the sandbox does not start here ({e})")),
        }
    }

    /// Whether runs are confined.
    #[cfg(test)]
    pub(crate) fn on(&self) -> bool {
        !matches!(self, Confine::Off(_))
    }

    /// The sandbox runs get (`seatbelt`, `bubblewrap`), or why they get none.
    pub(crate) fn state(&self) -> Result<&'static str, String> {
        match self {
            Confine::Off(why) => Err(why.clone()),
            Confine::Seatbelt(_) => Ok("seatbelt"),
            Confine::Bwrap { .. } => Ok("bubblewrap"),
        }
    }

    /// The seatbelt rules, to append to a profile a run already has (a job's or research's own).
    pub(crate) fn seatbelt_rules(&self) -> Option<&str> {
        match self {
            Confine::Seatbelt(r) => Some(r),
            _ => None,
        }
    }

    /// `program args` run in the sandbox; `readable`: directories it may read although they are
    /// under what is hidden (a command's own secret files).
    pub(crate) fn wrap(
        &self,
        program: PathBuf,
        args: Vec<OsString>,
        readable: &[PathBuf],
    ) -> (PathBuf, Vec<OsString>) {
        match self {
            Confine::Off(_) => (program, args),
            Confine::Seatbelt(rules) => {
                let mut profile = format!("(version 1)\n(allow default)\n{rules}");
                for r in readable {
                    let real = std::fs::canonicalize(r).unwrap_or_else(|_| r.clone());
                    for r in [r, &real] {
                        let r = r.to_string_lossy();
                        if !r.contains(['"', '\\']) {
                            profile.push_str(&format!("(allow file-read* (subpath \"{r}\"))\n"));
                        }
                    }
                }
                let mut a: Vec<OsString> = vec!["-p".into(), profile.into(), program.into()];
                a.extend(args);
                (SANDBOX_EXEC.into(), a)
            }
            Confine::Bwrap {
                bwrap,
                steve_home,
                node_home,
            } => {
                let (hidden, sealed) = steve_vault::harness::bwrap_paths(steve_home, node_home);
                let mut a: Vec<OsString> = vec!["--dev-bind".into(), "/".into(), "/".into()];
                for p in &sealed {
                    a.extend(["--ro-bind".into(), p.into(), p.into()]);
                }
                for p in &hidden {
                    if p.is_dir() {
                        a.extend(["--tmpfs".into(), p.into()]);
                    } else {
                        a.extend(["--ro-bind".into(), "/dev/null".into(), p.into()]);
                    }
                }
                for r in readable {
                    a.extend(["--ro-bind".into(), r.into(), r.into()]);
                }
                a.push("--".into());
                a.push(program.into());
                a.extend(args);
                (bwrap.clone(), a)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_when_asked_or_unknown() {
        let h = Path::new("/nonexistent-steve-home");
        assert!(!Confine::choose(h, h, Some("off".into())).on());
        let c = Confine::choose(h, h, Some("maybe".into()));
        assert!(
            matches!(&c, Confine::Off(why) if why.contains("maybe")),
            "{c:?}"
        );
        let (p, a) = c.wrap("/bin/echo".into(), vec!["x".into()], &[]);
        assert_eq!(
            (p, a),
            (PathBuf::from("/bin/echo"), vec![OsString::from("x")])
        );
    }

    #[test]
    fn bwrap_hides_dirs_with_tmpfs_and_files_with_null() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join("signing")).unwrap();
        std::fs::create_dir_all(home.join("releases")).unwrap();
        std::fs::write(home.join("steve.env"), "").unwrap();
        let c = Confine::Bwrap {
            bwrap: "/usr/bin/bwrap".into(),
            steve_home: home.clone(),
            node_home: home.join("data/node"),
        };
        let real = std::fs::canonicalize(&home).unwrap();
        let (p, a) = c.wrap("/bin/sh".into(), vec!["-c".into(), "x".into()], &[]);
        assert_eq!(p, PathBuf::from("/usr/bin/bwrap"));
        let a: Vec<String> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        let at = |s: &str| a.iter().position(|x| x == s).unwrap();
        let signing = real.join("signing").to_string_lossy().into_owned();
        let env = real.join("steve.env").to_string_lossy().into_owned();
        assert_eq!(a[at(&signing) - 1], "--tmpfs");
        assert_eq!(a[at(&env) - 2..at(&env)], ["--ro-bind", "/dev/null"]);
        assert_eq!(&a[a.len() - 4..], ["--", "/bin/sh", "-c", "x"]);
    }

    /// On this Mac: a run cannot read the Node's secrets but reads what it was given.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_wrapped_command_cannot_read_the_control_plane() {
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
    }
}
