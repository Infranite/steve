//! The OS sandbox around what a Node starts for a run: a session's harness, a job's harness, a
//! command. This is the first stage of "every run in the OS sandbox" (path model S12). Steve's
//! control plane stays out of the run's reach whatever the harness does with its own shell: the
//! Hub's and the Node's secrets, keys, databases and journal cannot be read or written, and
//! Steve's own programs cannot be changed (`steve_vault::harness::{PROTECTED, NODE_PROTECTED,
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

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The sandbox of this Node's runs: which one, how far it isolates files, and the environment
/// runs start from at level 2.
#[derive(Debug, Clone)]
pub(crate) struct Confine {
    sandbox: Sandbox,
    /// Writes of a run in a workspace are kept to its own places (level 1 and up).
    files: bool,
    /// Level 2: the user's fresh login environment.
    env: Option<std::sync::Arc<Vec<(OsString, OsString)>>>,
}

#[derive(Debug, Clone)]
pub(crate) enum Sandbox {
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
        let level = match (&c.sandbox, c.files, c.env.is_some()) {
            (Sandbox::Off(_), ..) => 0,
            (_, _, true) => 2,
            (_, true, _) => 1,
            _ => 0,
        };
        match &c.sandbox {
            Sandbox::Off(why) => tracing::warn!("node: runs are not confined: {why}"),
            Sandbox::Seatbelt(_) => {
                tracing::info!("node: runs are confined (seatbelt, level {level})")
            }
            Sandbox::Bwrap { .. } => {
                tracing::info!("node: runs are confined (bubblewrap, level {level})")
            }
        }
        c
    }

    /// Level 2: the environment a run starts from (then what Steve sets for it).
    pub(crate) fn fresh_env(&self) -> Option<&[(OsString, OsString)]> {
        self.env.as_deref().map(Vec::as_slice)
    }

    fn from_env(steve_home: &Path, node_home: &Path) -> Confine {
        Self::choose(steve_home, node_home, std::env::var("STEVE_CONFINE").ok())
    }

    pub(crate) fn choose(steve_home: &Path, node_home: &Path, mode: Option<String>) -> Confine {
        let off = |why: String| Confine {
            sandbox: Sandbox::Off(why),
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
        if matches!(sandbox, Sandbox::Off(_)) {
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

    fn sandbox(steve_home: &Path, node_home: &Path) -> Sandbox {
        let c = if cfg!(target_os = "macos") {
            if !Path::new(SANDBOX_EXEC).exists() {
                return Sandbox::Off(format!("{SANDBOX_EXEC} is missing"));
            }
            match steve_vault::harness::seatbelt_rules(steve_home, node_home) {
                Some(rules) => Sandbox::Seatbelt(rules),
                None => return Sandbox::Off("a Steve path cannot be written in a profile".into()),
            }
        } else if cfg!(target_os = "linux") {
            // bubblewrap, when installed (level 1 by default, as on macOS)
            let Some(bwrap) = ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"]
                .iter()
                .map(PathBuf::from)
                .find(|p| p.exists())
            else {
                return Sandbox::Off("bwrap (bubblewrap) is not installed".into());
            };
            Sandbox::Bwrap {
                bwrap,
                steve_home: steve_home.to_path_buf(),
                node_home: node_home.to_path_buf(),
            }
        } else {
            return Sandbox::Off("no sandbox on this platform".into());
        };
        let probe = Confine {
            sandbox: c.clone(),
            files: false,
            env: None,
        };
        let (program, args) = probe.wrap("/usr/bin/true".into(), vec![], &[]);
        match std::process::Command::new(&program)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(o) if o.status.success() => c,
            Ok(o) => Sandbox::Off(format!(
                "the sandbox does not start here ({}: {})",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Sandbox::Off(format!("the sandbox does not start here ({e})")),
        }
    }

    /// Whether runs are confined.
    #[cfg(test)]
    pub(crate) fn on(&self) -> bool {
        !matches!(self.sandbox, Sandbox::Off(_))
    }

    /// The sandbox runs get (`seatbelt`, `bubblewrap`), or why they get none.
    pub(crate) fn state(&self) -> Result<&'static str, String> {
        match &self.sandbox {
            Sandbox::Off(why) => Err(why.clone()),
            Sandbox::Seatbelt(_) => Ok("seatbelt"),
            Sandbox::Bwrap { .. } => Ok("bubblewrap"),
        }
    }

    /// The seatbelt rules, to append to a profile a run already has (a job's or research's own).
    pub(crate) fn seatbelt_rules(&self) -> Option<&str> {
        match &self.sandbox {
            Sandbox::Seatbelt(r) => Some(r),
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
        self.wrap_in(program, args, readable, &[], None)
    }

    /// [`Self::wrap`] for a run that works in `cwd`: bubblewrap changes into it itself, so a
    /// directory the sandbox hides fails the start instead of leaving the run in `$HOME`.
    /// `writable`: directories it may read and write although they are under what is hidden (a
    /// session's own harness directory, under the Node's `state/`).
    pub(crate) fn wrap_in(
        &self,
        program: PathBuf,
        args: Vec<OsString>,
        readable: &[PathBuf],
        writable: &[PathBuf],
        cwd: Option<&Path>,
    ) -> (PathBuf, Vec<OsString>) {
        // the places a run in a workspace writes, level 1 and up
        let writes = match cwd {
            Some(cwd) if self.files => Some(writable_places(cwd, writable)),
            _ => None,
        };
        match &self.sandbox {
            Sandbox::Off(_) => (program, args),
            Sandbox::Seatbelt(rules) => {
                let mut profile = String::from("(version 1)\n(allow default)\n");
                if let Some(places) = &writes {
                    profile.push_str(&seatbelt_writes(places));
                }
                profile.push_str(rules);
                let opened = readable
                    .iter()
                    .map(|r| (r, "file-read*"))
                    .chain(writable.iter().map(|w| (w, "file-read* file-write*")));
                for (r, ops) in opened {
                    let real = std::fs::canonicalize(r).unwrap_or_else(|_| r.clone());
                    for r in [r, &real] {
                        let r = r.to_string_lossy();
                        if !r.contains(['"', '\\']) {
                            profile.push_str(&format!("(allow {ops} (subpath \"{r}\"))\n"));
                        }
                    }
                }
                let mut a: Vec<OsString> = vec!["-p".into(), profile.into(), program.into()];
                a.extend(args);
                (SANDBOX_EXEC.into(), a)
            }
            Sandbox::Bwrap {
                bwrap,
                steve_home,
                node_home,
            } => {
                // what the Node makes for runs and later for itself (its secrets, pushes,
                // attachments) exists first: open or hidden from the start
                // the Hub's own home (its local Node lives in it): its run directories exist
                if node_home.starts_with(steve_home.join("data")) {
                    for d in steve_vault::harness::RUN_DIRS {
                        let _ = std::fs::create_dir_all(steve_home.join(d));
                    }
                }
                for d in steve_vault::harness::NODE_PROTECTED {
                    if let Some(d) = d.strip_suffix('/') {
                        let _ = std::fs::DirBuilder::new()
                            .recursive(true)
                            .mode(0o700)
                            .create(node_home.join(d));
                    }
                }
                let p = steve_vault::harness::bwrap_plan(steve_home, node_home);
                let mut a: Vec<OsString> = match &writes {
                    // read-only but for the run's own places
                    Some(places) => {
                        let mut a: Vec<OsString> = vec![
                            "--ro-bind".into(),
                            "/".into(),
                            "/".into(),
                            "--dev".into(),
                            "/dev".into(),
                            "--proc".into(),
                            "/proc".into(),
                        ];
                        for w in places.dirs.iter().filter(|d| d.is_dir()) {
                            a.extend(["--bind".into(), w.into(), w.into()]);
                        }
                        for f in places.files.iter().filter(|f| f.is_file()) {
                            a.extend(["--bind".into(), f.into(), f.into()]);
                        }
                        a
                    }
                    None => vec!["--dev-bind".into(), "/".into(), "/".into()],
                };
                for d in &p.masked {
                    a.extend(["--tmpfs".into(), d.into()]);
                }
                for d in &p.writable {
                    a.extend(["--bind".into(), d.into(), d.into()]);
                }
                for d in &p.hidden_dirs {
                    if d.is_dir() {
                        a.extend(["--tmpfs".into(), d.into()]);
                    }
                }
                for f in &p.hidden_files {
                    a.extend(["--ro-bind".into(), "/dev/null".into(), f.into()]);
                }
                for r in readable {
                    let r = std::fs::canonicalize(r).unwrap_or_else(|_| r.clone());
                    a.extend(["--ro-bind".into(), r.clone().into(), r.into()]);
                }
                for w in writable {
                    let w = std::fs::canonicalize(w).unwrap_or_else(|_| w.clone());
                    a.extend(["--bind".into(), w.clone().into(), w.into()]);
                }
                if let Some(cwd) = cwd {
                    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
                    a.extend(["--chdir".into(), cwd.into()]);
                }
                a.push("--".into());
                a.push(program.into());
                a.extend(args);
                (bwrap.clone(), a)
            }
        }
    }
}

/// Where a run in a workspace may write (level 1 and up).
#[derive(Debug, Default)]
pub(crate) struct Places {
    pub dirs: Vec<PathBuf>,
    /// Single files (the harness's own settings file in the home).
    pub files: Vec<PathBuf>,
}

/// The caches and harness homes under the user's home a run may write: what tools write as a
/// matter of course, so isolating files does not break them.
const HOME_WRITABLE: &[&str] = &[
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
    ".local/state",
    ".config",
    ".claude",
    ".codex",
];

fn writable_places(cwd: &Path, writable: &[PathBuf]) -> Places {
    let mut dirs: Vec<PathBuf> = vec![cwd.to_path_buf()];
    // a worktree's commits go into the repository it belongs to
    if let Some(common) = git_common_dir(cwd) {
        dirs.push(common);
    }
    dirs.extend(writable.iter().cloned());
    for t in ["/tmp", "/private/tmp", "/var/tmp", "/private/var/folders"] {
        dirs.push(PathBuf::from(t));
    }
    if let Some(t) = std::env::var_os("TMPDIR") {
        dirs.push(PathBuf::from(t));
    }
    let mut files = vec![];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend(HOME_WRITABLE.iter().map(|d| home.join(d)));
        files.push(home.join(".claude.json"));
    }
    if let Some(extra) = std::env::var_os("STEVE_CONFINE_WRITABLE") {
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

/// Seatbelt rules for [`Places`]: no write anywhere, then each place again (both as named and
/// resolved, `/tmp` being `/private/tmp`). Rules that come after (Steve's own denials, a run's
/// own openings) still apply over these.
fn seatbelt_writes(places: &Places) -> String {
    let mut s = String::from(
        "(deny file-write*)\n(allow file-write* (literal \"/dev/null\") (subpath \"/dev\"))\n",
    );
    let mut seen = std::collections::HashSet::new();
    for (p, kind) in places
        .dirs
        .iter()
        .map(|d| (d, "subpath"))
        .chain(places.files.iter().map(|f| (f, "file")))
    {
        let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        for p in [p.clone(), real] {
            let p = p.to_string_lossy().into_owned();
            if p.contains(['"', '\\']) || !seen.insert((p.clone(), kind)) {
                continue;
            }
            if kind == "subpath" {
                s.push_str(&format!("(allow file-write* (subpath \"{p}\"))\n"));
            } else {
                // the file, and the temporary siblings it is written through
                s.push_str(&format!(
                    "(allow file-write* (regex #\"^{}\"))\n",
                    regex_escape(&p)
                ));
            }
        }
    }
    s
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            let special = ".^$*+?()[]{}|\\/".contains(c);
            special
                .then_some('\\')
                .into_iter()
                .chain(std::iter::once(c))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_when_asked_or_unknown() {
        let h = Path::new("/nonexistent-steve-home");
        assert!(!Confine::choose(h, h, Some("off".into())).on());
        let c = Confine::choose(h, h, Some("maybe".into()));
        assert!(
            matches!(&c.sandbox, Sandbox::Off(why) if why.contains("maybe")),
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
            sandbox: Sandbox::Bwrap {
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
    #[test]
    fn a_wrapped_command_cannot_read_the_control_plane() {
        if !steve_vault::harness::seatbelt_works() {
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
    }

    /// On a Linux machine with bubblewrap: the Hub's home is hidden whole, files the Hub writes
    /// there after the run started included, the run directories stay writable, a command's own
    /// secrets stay readable, and `/proc/<parent>/root` is no way around it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_wrapped_command_cannot_reach_the_control_plane_on_linux() {
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
        let (p, a) = c.wrap_in(
            "/bin/sh".into(),
            vec![
                "-c".into(),
                format!("sleep 1; cat {h}/steve.env.new").into(),
            ],
            &[],
            &[],
            Some(&home.join("worktrees/ST-1")),
        );
        let child = std::process::Command::new(p)
            .args(a)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(home.join("steve.env.new"), "late").unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(!o.status.success(), "a late file was readable: {o:?}");
    }

    /// Level 1 on this Mac: a run writes in its own places and nowhere else; reads stay open.
    #[cfg(target_os = "macos")]
    #[test]
    fn level_one_keeps_writes_to_the_runs_places() {
        if !steve_vault::harness::seatbelt_works() {
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
}
