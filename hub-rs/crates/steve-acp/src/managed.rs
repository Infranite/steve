//! The built-in adapters as Steve's own dependencies: installed with npm into `<home>/adapters`
//! (a prefix of their own, not the global one), found there first, and kept up to date with the
//! npm registry (the user's npm configuration, so a mirror works). No version is pinned: the latest
//! release is what Steve runs.
//!
//! - `steve setup` installs the ones its user chose ([`install`]) and records the choice
//!   ([`selection`]): an adapter that was not chosen is not installed later either;
//! - a Node checks for newer releases once a day and upgrades an adapter none of its sessions is
//!   using ([`Updates`]), as `STEVE_ADAPTER_UPDATES` says (`auto`, the default; `check`; `off`);
//! - `steve adapters status|upgrade` does the same by hand, and `steve adapters install <id>` adds
//!   one.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use steve_i18n::t;

/// The directory under a Steve home (a Hub's or a Node's) the adapters are installed in.
pub fn dir(home: &Path) -> PathBuf {
    home.join("adapters")
}

/// Its `node_modules`: what [`crate::adapters::builtin_commands`] takes as the npm root.
pub fn root(home: &Path) -> PathBuf {
    dir(home).join("node_modules")
}

/// The command of npm-managed built-in `id` in `home`'s adapters, whether it is installed or not.
pub fn command(home: &Path, id: &str) -> Option<PathBuf> {
    if !crate::adapters::is_npm_managed(id) {
        return None;
    }
    crate::adapters::builtin_commands(&root(home))
        .find(|(b, ..)| *b == id)
        .map(|(.., c)| c)
}

/// The npm package of built-in `id`.
pub fn package(id: &str) -> Option<&'static str> {
    crate::adapters::builtin_packages()
        .find(|(b, _)| *b == id)
        .map(|(_, p)| p)
}

/// The Steve home whose adapters `command` is installed in (`<home>/adapters/node_modules/…`).
pub fn home_of(command: &Path) -> Option<PathBuf> {
    let mut p = command;
    while let Some(parent) = p.parent() {
        if p.file_name().is_some_and(|n| n == "node_modules")
            && parent.file_name().is_some_and(|n| n == "adapters")
        {
            return parent.parent().map(Path::to_path_buf);
        }
        p = parent;
    }
    None
}

fn selection_file(home: &Path) -> PathBuf {
    dir(home).join("selected.json")
}

/// The built-in adapters `home` was asked to have (`steve setup`'s choice, and what
/// `adapters install <id>` added since). `None`: nothing was ever chosen, which means all of them.
pub fn selection(home: &Path) -> Option<Vec<String>> {
    serde_json::from_slice(&std::fs::read(selection_file(home)).ok()?).ok()
}

fn select(home: &Path, ids: &[&str]) -> Result<(), String> {
    let file = selection_file(home);
    std::fs::create_dir_all(dir(home)).map_err(|e| format!("{}: {e}", dir(home).display()))?;
    let text = serde_json::to_string(ids).map_err(|e| e.to_string())?;
    std::fs::write(&file, text + "\n").map_err(|e| format!("{}: {e}", file.display()))
}

/// Whether Steve keeps npm-managed built-in `id` in `home`: it is installed there, was chosen, or
/// nothing was ever chosen and `id` is in [`crate::adapters::DEFAULT_MANAGED_ADAPTERS`]. An adapter
/// its user left out (or one not distributed on npm) is neither installed by the daily check nor
/// reported as missing.
pub fn wanted(home: &Path, id: &str) -> bool {
    let Some(pkg) = package(id) else {
        return false;
    };
    installed(home, pkg).is_some()
        || match selection(home) {
            Some(chosen) => chosen.iter().any(|c| c == id),
            None => crate::adapters::DEFAULT_MANAGED_ADAPTERS.contains(&id),
        }
}

/// The version of `package` installed in `home`'s adapters.
pub fn installed(home: &Path, package: &str) -> Option<String> {
    let manifest = root(home).join(package).join("package.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest).ok()?).ok()?;
    v["version"].as_str().map(str::to_string)
}

/// `npm` on `PATH`, or beside `node` when that is given.
pub fn npm(node: Option<&Path>) -> Option<PathBuf> {
    let beside = node
        .and_then(|n| n.parent())
        .map(|d| d.join("npm"))
        .filter(|p| crate::config::is_executable_file(p));
    beside.or_else(|| crate::config::which_in("npm", std::env::var_os("PATH").as_deref()))
}

/// How long one npm call may take (an install downloads a few hundred MB).
const VIEW_TIMEOUT: Duration = Duration::from_secs(60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

fn run(
    npm: &Path,
    args: &[&str],
    env: &[(&str, &Path)],
    timeout: Duration,
) -> Result<String, String> {
    let mut cmd = Command::new(npm);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", npm.display()))?;
    let (out, err) = (child.stdout.take(), child.stderr.take());
    let read = |r: Option<std::process::ChildStdout>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let _ = std::io::Read::read_to_string(&mut r, &mut s);
            }
            s
        })
    };
    let out = read(out);
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut r) = err {
            let _ = std::io::Read::read_to_string(&mut r, &mut s);
        }
        s
    });
    let until = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if std::time::Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "npm {} took longer than {timeout:?}",
                args.join(" ")
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let (out, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    if status.success() {
        Ok(out)
    } else {
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        Err(format!("npm {}: {status}: {last}", args.join(" ")))
    }
}

/// The latest release of `package` on the registry npm is configured with.
pub fn latest(npm: &Path, package: &str) -> Result<String, String> {
    let out = run(
        npm,
        &["view", package, "version", "--json"],
        &[],
        VIEW_TIMEOUT,
    )?;
    serde_json::from_str::<String>(out.trim()).map_err(|_| format!("npm view {package}: {out}"))
}

/// Installs (or upgrades to) the latest release of each of `packages` into `home`'s adapters.
pub fn install(npm: &Path, home: &Path, packages: &[&str]) -> Result<(), String> {
    let dir = dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let grok_install = dir.join(".grok-install");
    let prefix = dir.to_string_lossy().into_owned();
    let specs: Vec<String> = packages.iter().map(|p| format!("{p}@latest")).collect();
    let mut args = vec!["install", "--prefix", &prefix, "--no-audit", "--no-fund"];
    args.extend(specs.iter().map(String::as_str));
    run(
        npm,
        &args,
        &[("GROK_HOME", grok_install.as_path())],
        INSTALL_TIMEOUT,
    )
    .map(|_| ())
}

/// One built-in adapter as `home` has it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    pub id: &'static str,
    pub package: &'static str,
    pub installed: Option<String>,
    /// `None`: not asked, or the registry could not say (`error`).
    pub latest: Option<String>,
    pub error: Option<String>,
}

impl Status {
    pub fn outdated(&self) -> bool {
        matches!((&self.installed, &self.latest), (Some(i), Some(l)) if newer(l, i))
            || (self.installed.is_none() && self.error.is_none())
    }
}

/// Each built-in adapter in `home`, with the latest release when `npm` is given and the adapter is
/// wanted in `home` (installed or selected).
pub fn status(home: &Path, npm: Option<&Path>) -> Vec<Status> {
    crate::adapters::builtin_packages()
        .map(|(id, package)| {
            let inst = installed(home, package);
            let query = npm.filter(|_| inst.is_some() || wanted(home, id));
            let (latest, error) = match query.map(|n| latest(n, package)) {
                Some(Ok(v)) => (Some(v), None),
                Some(Err(e)) => (None, Some(e)),
                None => (None, None),
            };
            Status {
                id,
                package,
                installed: inst,
                latest,
                error,
            }
        })
        .collect()
}

/// Whether version `a` is after `b` (`major.minor.patch`, SemVer 2.0 §11 pre-release ordering,
/// `+build` metadata ignored).
pub fn newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, Option<&str>) {
        let no_build = v
            .trim()
            .trim_start_matches('v')
            .split('+')
            .next()
            .unwrap_or("");
        let (core, pre) = match no_build.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p)),
            Some((c, _)) => (c, None),
            None => (no_build, None),
        };
        (
            core.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    fn cmp_pre(a: &str, b: &str) -> std::cmp::Ordering {
        let mut ai = a.split('.');
        let mut bi = b.split('.');
        loop {
            match (ai.next(), bi.next()) {
                (None, None) => return std::cmp::Ordering::Equal,
                (None, Some(_)) => return std::cmp::Ordering::Less,
                (Some(_), None) => return std::cmp::Ordering::Greater,
                (Some(x), Some(y)) => {
                    let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                        (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                        (Err(_), Err(_)) => x.cmp(y),
                    };
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                }
            }
        }
    }
    let ((a, a_pre), (b, b_pre)) = (parts(a), parts(b));
    match a.cmp(&b) {
        std::cmp::Ordering::Equal => match (a_pre, b_pre) {
            (None, None) => false,
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (Some(ap), Some(bp)) => cmp_pre(ap, bp) == std::cmp::Ordering::Greater,
        },
        o => o == std::cmp::Ordering::Greater,
    }
}

/// `STEVE_ADAPTER_UPDATES`: what a Node does about newer adapter releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePolicy {
    /// Check once a day and upgrade an adapter no session is using (the default).
    Auto,
    /// Check and show; upgrade only by hand (`steve adapters upgrade`).
    Check,
    /// Neither.
    Off,
}

impl UpdatePolicy {
    pub fn from_env() -> UpdatePolicy {
        match std::env::var("STEVE_ADAPTER_UPDATES")
            .as_deref()
            .map(str::trim)
        {
            Ok("check") => UpdatePolicy::Check,
            Ok("off") | Ok("0") => UpdatePolicy::Off,
            _ => UpdatePolicy::Auto,
        }
    }
}

/// `steve-hub --adapters …` and `steve-node adapters …` (what `steve setup` and `steve adapters`
/// run): `status [--json]` (installed and latest), `list --json` (what there is to choose from,
/// without asking the registry), `install [<id>…]` (the latest of each of those, also to upgrade;
/// without ids, the ones this home keeps) and `install --only [<id>…]` (the same, and exactly
/// those are the choice from now on: none, without ids).
/// The exit code: 0, 1 (npm failed), 2 (usage, or no npm).
pub fn cli(home: &Path, node: Option<&Path>, args: &[String]) -> i32 {
    steve_i18n::set_process(steve_i18n::from_env());
    let json = args.iter().any(|a| a == "--json");
    let npm = npm(node);
    match args.first().map(String::as_str) {
        Some("status") | None => {
            let list = status(home, npm.as_deref());
            if json {
                println!("{}", serde_json::to_string(&list).unwrap_or_default());
            } else {
                for s in &list {
                    let state = match (&s.installed, &s.latest, &s.error) {
                        (None, _, _) if !wanted(home, s.id) => t!("acp.managed.not_selected"),
                        (None, None, Some(e)) => {
                            t!("acp.managed.not_installed_error", error = e)
                        }
                        (None, _, _) => t!("acp.managed.not_installed"),
                        (Some(i), Some(l), _) if newer(l, i) => {
                            t!("acp.managed.upgradable", installed = i, latest = l)
                        }
                        (Some(i), Some(_), _) => t!("acp.managed.up_to_date", installed = i),
                        (Some(i), None, Some(e)) => {
                            t!("acp.managed.latest_unknown", installed = i, error = e)
                        }
                        (Some(i), None, None) => i.clone(),
                    };
                    println!("{} ({}): {state}", s.id, s.package);
                }
                if npm.is_none() {
                    println!("{}", no_npm());
                }
            }
            0
        }
        Some("list") => {
            let path_env = std::env::var_os("PATH");
            let adapters: Vec<serde_json::Value> = crate::adapters::builtin_packages()
                .map(|(id, package)| {
                    serde_json::json!({
                        "id": id,
                        "name": crate::adapters::spec(id, None).name,
                        "package": package,
                        "installed": installed(home, package),
                        "selected": wanted(home, id),
                        "detected": crate::adapters::is_cli_detected(id, path_env.as_deref()),
                    })
                })
                .collect();
            let path_only: Vec<&str> = crate::adapters::builtin_ids()
                .filter(|id| !crate::adapters::is_npm_managed(id))
                .collect();
            let detected_path_only: Vec<&str> = path_only
                .iter()
                .copied()
                .filter(|id| crate::adapters::is_cli_detected(id, path_env.as_deref()))
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "npm": npm.is_some(),
                    "chosen": selection(home).is_some(),
                    "adapters": adapters,
                    "path_only": path_only,
                    "detected_path_only": detected_path_only,
                })
            );
            0
        }
        Some(cmd @ ("install" | "upgrade")) => {
            let is_upgrade = cmd == "upgrade";
            let ids: Vec<&str> = args[1..]
                .iter()
                .map(String::as_str)
                .filter(|a| !a.starts_with("--"))
                .collect();
            if is_upgrade && !ids.is_empty() {
                eprintln!("{}", t!("acp.managed.usage"));
                return 2;
            }
            if let Some(unknown) = ids.iter().find(|id| package(id).is_none()) {
                let spec = crate::adapters::spec(unknown, None);
                if spec.builtin {
                    let hint = spec.install_hint.unwrap_or_default();
                    eprintln!(
                        "{}",
                        t!("acp.managed.not_npm_managed", id = unknown, hint = hint)
                    );
                } else {
                    let known: Vec<&str> = crate::adapters::builtin_packages()
                        .map(|(id, _)| id)
                        .collect();
                    eprintln!(
                        "{}",
                        t!(
                            "acp.managed.unknown_adapter",
                            unknown = unknown,
                            known = known.join(&t!("acp.managed.list_separator"))
                        )
                    );
                }
                return 2;
            }
            /* --only: exactly these are chosen (none, without ids); otherwise named ids join an
             * earlier choice after they install, and without ids what is wanted is brought up to date */
            let only = !is_upgrade && args.iter().any(|a| a == "--only");
            let before = selection(home);
            if only && let Err(e) = select(home, &ids) {
                eprintln!("{e}");
                return 1;
            }
            let targets: Vec<&str> = crate::adapters::builtin_packages()
                .map(|(id, _)| id)
                .filter(|id| match only || !ids.is_empty() {
                    true => ids.contains(id),
                    false => wanted(home, id),
                })
                .collect();
            if targets.is_empty() {
                println!("{}", t!("acp.managed.none_selected"));
                return 0;
            }
            let Some(npm) = npm else {
                eprintln!("{}", no_npm());
                return 2;
            };
            let packages: Vec<&str> = targets.iter().filter_map(|id| package(id)).collect();
            println!(
                "{}",
                t!(
                    "acp.managed.installing",
                    dir = dir(home).display(),
                    packages = packages.join(&t!("acp.managed.list_separator"))
                )
            );
            let mut installed_ids: Vec<&str> = Vec::new();
            let mut failed = false;
            match install(&npm, home, &packages) {
                Ok(()) => installed_ids.extend_from_slice(&targets),
                Err(e) if packages.len() <= 1 => {
                    eprintln!("{}", t!("acp.managed.install_failed", error = e));
                    failed = true;
                }
                Err(_) => {
                    for &id in &targets {
                        if let Some(pkg) = package(id) {
                            match install(&npm, home, &[pkg]) {
                                Ok(()) => installed_ids.push(id),
                                Err(e) => {
                                    eprintln!("{}", t!("acp.managed.install_failed", error = e));
                                    failed = true;
                                }
                            }
                        }
                    }
                }
            }
            if !only
                && !is_upgrade
                && !installed_ids.is_empty()
                && let Some(prev) = before.as_ref()
            {
                let merged: Vec<&str> = crate::adapters::builtin_packages()
                    .map(|(id, _)| id)
                    .filter(|id| installed_ids.contains(&id) || prev.iter().any(|b| b == id))
                    .collect();
                if let Err(e) = select(home, &merged) {
                    eprintln!("{e}");
                    return 1;
                }
            }
            if !installed_ids.is_empty() {
                for s in status(home, None)
                    .iter()
                    .filter(|s| installed_ids.contains(&s.id))
                {
                    println!(
                        "{} ({}): {}",
                        s.id,
                        s.package,
                        s.installed
                            .as_deref()
                            .unwrap_or(&t!("acp.managed.not_installed"))
                    );
                }
            }
            if failed { 1 } else { 0 }
        }
        _ => {
            eprintln!("{}", t!("acp.managed.usage"));
            2
        }
    }
}

/// What to do without npm.
pub fn no_npm() -> String {
    t!("acp.managed.npm_missing")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_then_release() {
        assert!(newer("0.102.0", "0.99.1"));
        assert!(newer("2.6.0", "0.16.0"));
        assert!(!newer("0.99.1", "0.99.1"));
        assert!(!newer("0.99.0", "0.99.1"));
        assert!(newer("1.0.0", "1.0.0-beta.2"));
        assert!(!newer("1.0.0-beta.2", "1.0.0"));
    }

    #[test]
    fn adapters_are_found_in_their_own_prefix() {
        let home = tempfile::tempdir().unwrap();
        let claude = command(home.path(), "claude-code").unwrap();
        assert_eq!(
            claude,
            home.path()
                .join("adapters/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js")
        );
        let pkg = root(home.path()).join("@agentclientprotocol/claude-agent-acp");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), r#"{"version": "0.102.0"}"#).unwrap();
        assert_eq!(
            installed(home.path(), "@agentclientprotocol/claude-agent-acp").as_deref(),
            Some("0.102.0")
        );
        let s = status(home.path(), None);
        assert_eq!(s.len(), crate::adapters::builtin_packages().count());
        assert!(!s[0].outdated(), "nothing asked: not outdated");
        assert!(s[1].outdated(), "not installed: to install");
    }

    fn add(home: &Path, id: &str, version: &str) {
        let pkg = root(home).join(package(id).unwrap());
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            format!(r#"{{"version": "{version}"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn what_setup_chose_is_what_a_home_keeps() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let ids: Vec<&str> = crate::adapters::builtin_packages()
            .map(|(id, _)| id)
            .collect();
        let (first, second, third) = (ids[0], ids[1], ids[2]);
        // nothing was ever chosen: only DEFAULT_MANAGED_ADAPTERS are wanted by default; PATH-only ones are never npm-wanted
        assert_eq!(selection(home), None);
        assert!(wanted(home, first) && wanted(home, second) && !wanted(home, third));
        assert!(!wanted(home, "cursor"));
        select(home, &[first]).unwrap();
        assert_eq!(selection(home), Some(vec![first.to_string()]));
        assert!(wanted(home, first) && !wanted(home, second));
        // one installed by hand is kept whatever was chosen
        add(home, second, "1.0.0");
        assert!(wanted(home, second));
        // "none" is a choice too
        let bare = tempfile::tempdir().unwrap();
        select(bare.path(), &[]).unwrap();
        assert_eq!(selection(bare.path()), Some(vec![]));
        assert!(!ids.iter().any(|id| wanted(bare.path(), id)));
        // a file that is not a list is no choice
        std::fs::write(selection_file(bare.path()), "{").unwrap();
        assert_eq!(selection(bare.path()), None);
    }

    #[test]
    fn the_home_of_an_adapter_is_the_one_it_is_installed_in() {
        let home = Path::new("/u/.steve");
        for (id, _) in crate::adapters::builtin_packages() {
            assert_eq!(home_of(&command(home, id).unwrap()).as_deref(), Some(home));
        }
        assert_eq!(command(home, "cursor"), None);
        assert_eq!(home_of(&root(home)).as_deref(), Some(home));
        // a global install, a bare command, and an `adapters` that is not a prefix
        assert_eq!(
            home_of(Path::new("/opt/homebrew/lib/node_modules/x/dist/index.js")),
            None
        );
        assert_eq!(home_of(Path::new("some-acp")), None);
        assert_eq!(home_of(Path::new("/u/.steve/adapters/x/index.js")), None);
    }

    /// An `npm` beside a `node` that answers `view` with 9.9.9, "installs" by writing each
    /// package's manifest under the prefix, and records what it was asked.
    fn fake_npm(dir: &Path) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let (npm, log) = (dir.join("npm"), dir.join("npm.log"));
        let script = format!(
            r#"#!/bin/sh
echo "$*" >> '{log}'
case "$1" in
  view) echo '"9.9.9"' ;;
  install)
    for a in "$@"; do
      case "$a" in *@latest)
        p="$3/node_modules/${{a%@latest}}"; mkdir -p "$p"; echo '{{"version": "9.9.9"}}' > "$p/package.json" ;;
      esac
    done ;;
  *) exit 64 ;;
esac
"#,
            log = log.display()
        );
        std::fs::write(&npm, script).unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir.join("node"), log)
    }

    #[test]
    fn the_command_installs_what_is_named_and_records_the_choice() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let (node, log) = fake_npm(temp.path());
        let ids: Vec<&str> = crate::adapters::builtin_packages()
            .map(|(id, _)| id)
            .collect();
        let (first, second) = (ids[0], ids[1]);
        let run = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            cli(&home, Some(&node), &args)
        };
        let asked = || std::fs::read_to_string(&log).unwrap_or_default();
        let version = |id: &str| installed(&home, package(id).unwrap());

        // an unknown or PATH-only id changes nothing
        assert_eq!(run(&["install", "no-such-adapter"]), 2);
        assert_eq!(run(&["install", "cursor"]), 2);
        assert_eq!(run(&["frobnicate"]), 2);
        assert!(!home.exists() && asked().is_empty());

        // --only: exactly these are the choice (what setup runs)
        assert_eq!(run(&["install", "--only", second]), 0);
        assert_eq!(selection(&home), Some(vec![second.to_string()]));
        assert_eq!(
            (version(first), version(second)),
            (None, Some("9.9.9".into()))
        );
        assert_eq!(asked().lines().count(), 1, "{}", asked());
        assert!(asked().contains(&format!("{}@latest", package(second).unwrap())));
        assert!(!asked().contains(package(first).unwrap()));

        // without ids, what the home keeps is brought up to date: the other one stays out
        assert_eq!(run(&["upgrade"]), 0);
        assert_eq!(version(first), None);

        // a named id joins the choice, in the built-in order
        assert_eq!(run(&["install", first]), 0);
        assert_eq!(
            selection(&home),
            Some(vec![first.to_string(), second.to_string()])
        );
        assert_eq!(version(first).as_deref(), Some("9.9.9"));

        // --only without ids: none, and npm is not asked
        let none = temp.path().join("none");
        let before = asked();
        let args = ["install".to_string(), "--only".to_string()];
        assert_eq!(cli(&none, Some(&node), &args), 0);
        assert_eq!(selection(&none), Some(vec![]));
        assert_eq!(asked(), before);
        assert!(!root(&none).exists());

        // a home where nothing was chosen gets the default managed adapters, and still records no choice
        let all = temp.path().join("all");
        assert_eq!(cli(&all, Some(&node), &["install".to_string()]), 0);
        assert_eq!(selection(&all), None);
        for id in &ids {
            assert_eq!(
                installed(&all, package(id).unwrap()).is_some(),
                crate::adapters::DEFAULT_MANAGED_ADAPTERS.contains(id),
                "{id}"
            );
        }

        // status only queries npm for wanted adapters in `home`, and list asks nothing
        let before = asked();
        assert_eq!(run(&["status", "--json"]), 0);
        assert_eq!(run(&["list", "--json"]), 0);
        assert_eq!(
            asked().lines().count(),
            before.lines().count() + 2,
            "status asks for each wanted adapter in home"
        );
        assert!(!asked()[before.len()..].contains("install"));
    }

    #[test]
    fn semver_prereleases_and_build_metadata_compare_in_order() {
        assert!(newer("1.0.0-alpha.2", "1.0.0-alpha.1"));
        assert!(newer("1.0.0-alpha.10", "1.0.0-alpha.2"));
        assert!(newer("1.0.0-beta.1", "1.0.0-alpha.9"));
        assert!(newer("1.0.0", "1.0.0-rc.1"));
        assert!(!newer("1.0.0-rc.1", "1.0.0"));
        assert!(!newer("1.2.3+build.9", "1.2.3+build.1"));
        assert!(newer("1.2.4+build.1", "1.2.3+build.9"));
    }

    #[test]
    fn partial_batch_failure_installs_healthy_adapters_and_only_selects_succeeded_ids() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let npm = temp.path().join("npm");
        let node = temp.path().join("node");
        let ids: Vec<&str> = crate::adapters::builtin_packages()
            .map(|(id, _)| id)
            .collect();
        let (first, second, third) = (ids[0], ids[1], ids[2]);
        let bad_pkg = package(third).unwrap();
        let script = format!(
            r#"#!/bin/sh
case "$1" in
  install)
    for a in "$@"; do
      case "$a" in "{bad}@latest") exit 1 ;; esac
    done
    for a in "$@"; do
      case "$a" in *@latest)
        p="$3/node_modules/${{a%@latest}}"; mkdir -p "$p"; echo '{{"version": "1.2.3"}}' > "$p/package.json" ;;
      esac
    done ;;
  *) exit 64 ;;
esac
"#,
            bad = bad_pkg
        );
        std::fs::write(&npm, script).unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();

        // upgrade rejects positional adapter IDs without touching selection
        assert_eq!(
            cli(&home, Some(&node), &["upgrade".into(), first.into()]),
            2
        );
        assert_eq!(selection(&home), None);

        // start with `first` selected
        assert_eq!(
            cli(
                &home,
                Some(&node),
                &["install".into(), "--only".into(), first.into()]
            ),
            0
        );
        assert_eq!(selection(&home), Some(vec![first.to_string()]));

        // installing `second` + failing `third` falls back per package: `second` installs and joins
        // `selected.json`, while `third` does not, and exit code is 1
        assert_eq!(
            cli(
                &home,
                Some(&node),
                &["install".into(), second.into(), third.into()]
            ),
            1
        );
        assert_eq!(
            installed(&home, package(second).unwrap()).as_deref(),
            Some("1.2.3")
        );
        assert_eq!(installed(&home, bad_pkg), None);
        assert_eq!(
            selection(&home),
            Some(vec![first.to_string(), second.to_string()])
        );
    }
}
