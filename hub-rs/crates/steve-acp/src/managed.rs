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
use steve_i18n::t;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The directory under a Steve home (a Hub's or a Node's) the adapters are installed in.
pub fn dir(home: &Path) -> PathBuf {
    home.join("adapters")
}

/// Its `node_modules`: what [`crate::adapters::builtin_commands`] takes as the npm root.
pub fn root(home: &Path) -> PathBuf {
    dir(home).join("node_modules")
}

/// The command of built-in `id` in `home`'s adapters, whether it is installed or not.
pub fn command(home: &Path, id: &str) -> Option<PathBuf> {
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

/// Whether Steve keeps built-in `id` in `home`: it is installed there, was chosen, or nothing was
/// ever chosen. An adapter its user left out is neither installed by the daily check nor reported
/// as missing.
pub fn wanted(home: &Path, id: &str) -> bool {
    package(id).is_some_and(|p| installed(home, p).is_some())
        || selection(home).is_none_or(|chosen| chosen.iter().any(|c| c == id))
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
        .filter(|p| p.is_file());
    beside.or_else(|| {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join("npm"))
            .find(|p| p.is_file())
    })
}

/// How long one npm call may take (an install downloads a few hundred MB).
const VIEW_TIMEOUT: Duration = Duration::from_secs(60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

fn run(npm: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(npm)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", npm.display()))?;
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
            return Err(format!("npm {} took longer than {timeout:?}", args.join(" ")));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() {
        Ok(out)
    } else {
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        Err(format!("npm {}: {status}: {last}", args.join(" ")))
    }
}

/// The latest release of `package` on the registry npm is configured with.
pub fn latest(npm: &Path, package: &str) -> Result<String, String> {
    let out = run(npm, &["view", package, "version", "--json"], VIEW_TIMEOUT)?;
    serde_json::from_str::<String>(out.trim()).map_err(|_| format!("npm view {package}: {out}"))
}

/// Installs (or upgrades to) the latest release of each of `packages` into `home`'s adapters.
pub fn install(npm: &Path, home: &Path, packages: &[&str]) -> Result<(), String> {
    let dir = dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let prefix = dir.to_string_lossy().into_owned();
    let specs: Vec<String> = packages.iter().map(|p| format!("{p}@latest")).collect();
    let mut args = vec!["install", "--prefix", &prefix, "--no-audit", "--no-fund"];
    args.extend(specs.iter().map(String::as_str));
    run(npm, &args, INSTALL_TIMEOUT).map(|_| ())
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

/// Each built-in adapter in `home`, with the latest release when `npm` is given.
pub fn status(home: &Path, npm: Option<&Path>) -> Vec<Status> {
    crate::adapters::builtin_packages()
        .map(|(id, package)| {
            let (latest, error) = match npm.map(|n| latest(n, package)) {
                Some(Ok(v)) => (Some(v), None),
                Some(Err(e)) => (None, Some(e)),
                None => (None, None),
            };
            Status {
                id,
                package,
                installed: installed(home, package),
                latest,
                error,
            }
        })
        .collect()
}

/// Whether version `a` is after `b` (`major.minor.patch`, a pre-release before its release).
pub fn newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, bool) {
        let (core, pre) = match v.trim().trim_start_matches('v').split_once('-') {
            Some((c, _)) => (c, true),
            None => (v.trim().trim_start_matches('v'), false),
        };
        (
            core.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    let ((a, a_pre), (b, b_pre)) = (parts(a), parts(b));
    match a.cmp(&b) {
        std::cmp::Ordering::Equal => b_pre && !a_pre,
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
        match std::env::var("STEVE_ADAPTER_UPDATES").as_deref().map(str::trim) {
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
                        (None, _, _) => t!("acp.managed.not_installed"),
                        (Some(i), Some(l), _) if newer(l, i) => t!("acp.managed.upgradable", installed = i, latest = l),
                        (Some(i), Some(_), _) => t!("acp.managed.up_to_date", installed = i),
                        (Some(i), None, Some(e)) => t!("acp.managed.latest_unknown", installed = i, error = e),
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
            let adapters: Vec<serde_json::Value> = crate::adapters::builtin_packages()
                .map(|(id, package)| {
                    serde_json::json!({
                        "id": id,
                        "name": crate::adapters::spec(id, None).name,
                        "package": package,
                        "installed": installed(home, package),
                        "selected": wanted(home, id),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::json!({"npm": npm.is_some(), "chosen": selection(home).is_some(), "adapters": adapters})
            );
            0
        }
        Some("install") | Some("upgrade") => {
            let ids: Vec<&str> = args[1..]
                .iter()
                .map(String::as_str)
                .filter(|a| !a.starts_with("--"))
                .collect();
            if let Some(unknown) = ids.iter().find(|id| package(id).is_none()) {
                let known: Vec<&str> = crate::adapters::builtin_ids().collect();
                eprintln!("{}", t!("acp.managed.unknown_adapter", unknown = unknown, known = known.join(&t!("acp.managed.list_separator"))));
                return 2;
            }
            /* --only: exactly these are chosen (none, without ids); otherwise named ids join an
             * earlier choice, and without ids what is wanted is brought up to date */
            let only = args.iter().any(|a| a == "--only");
            let recorded = match (only, selection(home)) {
                (true, _) => Some(ids.clone()),
                (false, Some(before)) if !ids.is_empty() => Some(
                    crate::adapters::builtin_ids()
                        .filter(|id| ids.contains(id) || before.iter().any(|b| b == id))
                        .collect(),
                ),
                _ => None,
            };
            if let Some(chosen) = recorded {
                if let Err(e) = select(home, &chosen) {
                    eprintln!("{e}");
                    return 1;
                }
            }
            let targets: Vec<&str> = crate::adapters::builtin_ids()
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
            println!("{}", t!("acp.managed.installing", dir = dir(home).display(), packages = packages.join(&t!("acp.managed.list_separator"))));
            match install(&npm, home, &packages) {
                Ok(()) => {
                    for s in status(home, None).iter().filter(|s| targets.contains(&s.id)) {
                        println!(
                            "{} ({}): {}",
                            s.id,
                            s.package,
                            s.installed.as_deref().unwrap_or(&t!("acp.managed.not_installed"))
                        );
                    }
                    0
                }
                Err(e) => {
                    eprintln!("{}", t!("acp.managed.install_failed", error = e));
                    1
                }
            }
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
        assert_eq!(s.len(), 2);
        assert!(!s[0].outdated(), "nothing asked: not outdated");
        assert!(s[1].outdated(), "not installed: to install");
    }
}
