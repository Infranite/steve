//! One policy renderer for seatbelt, and one for bubblewrap. Differences between a regular
//! run, a read-only run and a control-plane fragment are inputs, not separate implementations.
use super::{BwrapPlan, LoginStore, Places};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Default)]
enum Header {
    #[default]
    Expanded,
    Compact,
    None,
}
#[derive(Default)]
enum Writes<'a> {
    #[default]
    Ambient,
    Places(&'a Places),
    ReadOnly(&'a [PathBuf]),
}
#[derive(Default)]
enum Control<'a> {
    #[default]
    None,
    Homes(&'a Path, &'a Path),
    Fragment(&'a str),
}
#[derive(Default)]
struct SeatbeltPolicy<'a> {
    header: Header,
    writes: Writes<'a>,
    hidden: &'a [PathBuf],
    control: Control<'a>,
    readable: &'a [PathBuf],
    writable: &'a [PathBuf],
    login: Option<&'a LoginStore>,
}
fn render_seatbelt(policy: SeatbeltPolicy<'_>) -> Option<String> {
    let mut s = match policy.header {
        Header::Expanded => "(version 1)\n(allow default)\n".to_owned(),
        Header::Compact => "(version 1)(allow default)".to_owned(),
        Header::None => String::new(),
    };
    match policy.writes {
        Writes::Ambient => {}
        Writes::Places(places) => s.push_str(&seatbelt_writes(places)),
        Writes::ReadOnly(paths) => {
            s.push_str("(deny file-write*)\n(allow file-write*");
            for p in paths {
                let path = p.to_str()?;
                if path.contains(['"', '\\'])
                    || path.chars().any(char::is_control)
                    || !p.is_absolute()
                {
                    return None;
                }
                s.push_str(&format!(" (subpath \"{path}\")"));
            }
            s.push_str(" (literal \"/dev/null\") (literal \"/dev/tty\"))\n");
        }
    }
    s.push_str(&seatbelt_hide_cli(policy.hidden));
    match policy.control {
        Control::None => {}
        Control::Homes(steve, node) => s.push_str(&super::control::control_rules(steve, node)?),
        Control::Fragment(rules) => s.push_str(rules),
    }
    open_paths(&mut s, policy.readable, policy.writable);
    if let Some(login) = policy.login {
        s.push_str(&seatbelt_login(login));
    }
    Some(s)
}
fn open_paths(profile: &mut String, readable: &[PathBuf], writable: &[PathBuf]) {
    let opened = readable
        .iter()
        .map(|p| (p, "file-read*"))
        .chain(writable.iter().map(|p| (p, "file-read* file-write*")));
    for (r, ops) in opened {
        let real = std::fs::canonicalize(r).unwrap_or_else(|_| r.clone());
        // Keep both spellings and their original ordering, including duplicate named/real
        // entries: this preserves the previously generated policy byte for byte.
        for r in [r, &real] {
            for parent in r.ancestors().skip(1) {
                let parent = parent.to_string_lossy();
                if !parent.is_empty() && !parent.contains(['"', '\\']) {
                    profile.push_str(&format!(
                        "(allow file-read-metadata (literal \"{parent}\"))\n"
                    ));
                }
            }
            let r = r.to_string_lossy();
            if !r.contains(['"', '\\']) {
                profile.push_str(&format!("(allow {ops} (subpath \"{r}\"))\n"));
            }
        }
    }
}
pub(super) fn seatbelt_run(
    rules: &str,
    writes: Option<&Places>,
    hidden: &[PathBuf],
    readable: &[PathBuf],
    writable: &[PathBuf],
    login: Option<&LoginStore>,
) -> String {
    render_seatbelt(SeatbeltPolicy {
        writes: writes.map(Writes::Places).unwrap_or_default(),
        hidden,
        control: Control::Fragment(rules),
        readable,
        writable,
        login,
        ..Default::default()
    })
    .expect("validated run policy")
}
pub fn readonly_profile(writable: &[PathBuf]) -> Option<String> {
    render_seatbelt(SeatbeltPolicy {
        writes: Writes::ReadOnly(writable),
        ..Default::default()
    })
}
pub fn seatbelt_rules(steve: &Path, node: &Path) -> Option<String> {
    render_seatbelt(SeatbeltPolicy {
        header: Header::None,
        control: Control::Homes(steve, node),
        ..Default::default()
    })
}
pub(super) fn minimal_profile() -> String {
    render_seatbelt(SeatbeltPolicy {
        header: Header::Compact,
        ..Default::default()
    })
    .expect("fixed probe policy")
}

#[derive(Default, Clone, Copy)]
enum RootAccess<'a> {
    #[default]
    ReadOnly,
    Writable,
    Places(&'a Places),
}
#[derive(Default)]
struct BwrapPolicy<'a> {
    root: RootAccess<'a>,
    control: Option<&'a BwrapPlan>,
    readonly_runs: bool,
    hidden: &'a [PathBuf],
    login: Option<&'a LoginStore>,
    readable: &'a [PathBuf],
    writable: &'a [PathBuf],
    cwd: Option<&'a Path>,
    require_paths: bool,
}
fn render_bwrap(policy: BwrapPolicy<'_>, program: PathBuf, args: Vec<OsString>) -> Vec<OsString> {
    let mut a: Vec<OsString> = match policy.root {
        RootAccess::Writable => vec!["--dev-bind".into(), "/".into(), "/".into()],
        _ => vec![
            "--ro-bind".into(),
            "/".into(),
            "/".into(),
            "--dev".into(),
            "/dev".into(),
            "--proc".into(),
            "/proc".into(),
        ],
    };
    if let RootAccess::Places(places) = policy.root {
        for path in places
            .dirs
            .iter()
            .filter(|p| p.is_dir())
            .chain(places.files.iter().filter(|p| p.is_file()))
        {
            a.extend(["--bind".into(), path.into(), path.into()]);
        }
    }
    if let Some(p) = policy.control {
        for path in &p.masked {
            a.extend(["--tmpfs".into(), path.into()]);
        }
        let access = if policy.readonly_runs {
            "--ro-bind"
        } else {
            "--bind"
        };
        for path in &p.writable {
            a.extend([access.into(), path.into(), path.into()]);
        }
        for path in p.hidden_dirs.iter().filter(|p| p.is_dir()) {
            a.extend(["--tmpfs".into(), path.into()]);
        }
        for path in &p.hidden_files {
            a.extend(["--ro-bind".into(), "/dev/null".into(), path.into()]);
        }
    }
    bwrap_hide_cli(&mut a, policy.hidden);
    if let Some(login) = policy.login {
        bwrap_login(&mut a, login);
    }
    for path in policy.readable {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        a.extend(["--ro-bind".into(), path.clone().into(), path.into()]);
    }
    for path in policy.writable {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        if !policy.require_paths || path.is_dir() || path.is_file() {
            a.extend(["--bind".into(), path.clone().into(), path.into()]);
        }
    }
    if let Some(cwd) = policy.cwd {
        let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        if !policy.require_paths || cwd.is_dir() {
            a.extend(["--chdir".into(), cwd.into()]);
        }
    }
    a.push("--".into());
    a.push(program.into());
    a.extend(args);
    a
}
pub(super) fn bwrap_run(
    plan: &BwrapPlan,
    writes: Option<&Places>,
    hidden: &[PathBuf],
    readable: &[PathBuf],
    writable: &[PathBuf],
    cwd: Option<&Path>,
    login: Option<&LoginStore>,
    program: PathBuf,
    args: Vec<OsString>,
) -> Vec<OsString> {
    render_bwrap(
        BwrapPolicy {
            root: writes
                .map(RootAccess::Places)
                .unwrap_or(RootAccess::Writable),
            control: Some(plan),
            hidden,
            readable,
            writable,
            cwd,
            login,
            ..Default::default()
        },
        program,
        args,
    )
}
pub(super) fn bwrap_readonly(
    plan: Option<&BwrapPlan>,
    program: PathBuf,
    args: Vec<OsString>,
    writable: &[PathBuf],
    cwd: Option<&Path>,
    user_home: &Path,
) -> Vec<OsString> {
    let hidden: Vec<_> = super::SENSITIVE_HOME_DIRS
        .iter()
        .map(|p| user_home.join(p))
        .collect();
    render_bwrap(
        BwrapPolicy {
            control: plan,
            readonly_runs: true,
            hidden: &hidden,
            writable,
            cwd,
            require_paths: true,
            ..Default::default()
        },
        program,
        args,
    )
}
pub(super) fn minimal_bwrap_probe() -> Vec<OsString> {
    render_bwrap(BwrapPolicy::default(), "/usr/bin/true".into(), vec![])
}
/// Seatbelt: those names in the store, and whatever is under them (a lock directory), readable
/// and writable after everything that hides the directory; the way to it may be looked at.
pub(super) fn seatbelt_login(l: &LoginStore) -> String {
    let mut s = String::new();
    let names: Vec<String> = l
        .names
        .iter()
        .map(|n| match n.strip_suffix('*') {
            Some(prefix) => format!("{}[^/]*", regex_escape(prefix)),
            None => regex_escape(n),
        })
        .collect();
    if names.is_empty() {
        return s;
    }
    let real = std::fs::canonicalize(&l.dir).unwrap_or_else(|_| l.dir.clone());
    let mut seen = std::collections::HashSet::new();
    for d in [&l.dir, &real] {
        let ds = d.to_string_lossy().into_owned();
        if ds.contains(['"', '\\']) || ds.chars().any(char::is_control) || !seen.insert(ds.clone())
        {
            continue;
        }
        for p in d.ancestors() {
            let p = p.to_string_lossy();
            if !p.is_empty() && !p.contains(['"', '\\']) {
                s.push_str(&format!("(allow file-read-metadata (literal \"{p}\"))\n"));
            }
        }
        s.push_str(&format!(
            "(allow file-read* file-write* (regex #\"^{}/({})(/.*)?$\"))\n",
            regex_escape(&ds),
            names.join("|")
        ));
    }
    s
}

/// Bubblewrap: the store's own directory (it renames into it), with everything in it but those
/// names hidden again.
pub(super) fn bwrap_login(a: &mut Vec<OsString>, l: &LoginStore) {
    let Ok(dir) = std::fs::canonicalize(&l.dir) else {
        return;
    };
    if !dir.is_dir() || l.names.is_empty() {
        return;
    }
    a.extend(["--bind".into(), dir.clone().into(), dir.clone().into()]);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        if l.admits(&e.file_name().to_string_lossy()) {
            continue;
        }
        // what a link in it leads to is hidden only when it is in the directory too
        let Ok(real) = std::fs::canonicalize(e.path()) else {
            continue;
        };
        if !real.starts_with(&dir) {
            continue;
        }
        if real.is_dir() {
            a.extend(["--tmpfs".into(), real.into()]);
        } else if real.is_file() {
            a.extend(["--ro-bind".into(), "/dev/null".into(), real.into()]);
        }
    }
}

pub(super) fn seatbelt_hide_cli(paths: &[PathBuf]) -> String {
    let mut s = String::new();
    let mut seen = std::collections::HashSet::new();
    for p in paths {
        let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        for cand in [p, &real] {
            let cs = cand.to_string_lossy();
            if cs.contains(['"', '\\']) || cs.chars().any(char::is_control) {
                continue;
            }
            if seen.insert(cs.clone().into_owned()) {
                s.push_str(&format!(
                    "(deny file-read* file-write* (literal \"{cs}\") (subpath \"{cs}\"))\n"
                ));
            }
        }
    }
    s
}

pub(super) fn bwrap_hide_cli(a: &mut Vec<OsString>, paths: &[PathBuf]) {
    for p in paths {
        let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        if real.is_dir() {
            a.extend(["--tmpfs".into(), real.into()]);
        } else if real.is_file() {
            a.extend(["--ro-bind".into(), "/dev/null".into(), real.into()]);
        }
    }
}

/// Seatbelt rules for [`Places`]: no write anywhere, then each place again (both as named and
/// resolved, `/tmp` being `/private/tmp`). Rules that come after (Steve's own denials, a run's
/// own openings) still apply over these.
pub(super) fn seatbelt_writes(places: &Places) -> String {
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

pub(super) fn regex_escape(s: &str) -> String {
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
