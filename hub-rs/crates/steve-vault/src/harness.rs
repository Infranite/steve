//! What an agent harness Steve launches (Claude Code, Codex) must not see of the Hub: the files that
//! unlock the vault or hold the Hub's secrets, and the environment that points at them.
//!
//! Agents run as the owner's user, so these files are readable to them unless the harness itself
//! refuses. Not all of `STEVE_HOME`: item worktrees and the Node's scratch and job directories live
//! there and are the agents' own working directories.

use std::path::{Path, PathBuf};

/// The parts of `STEVE_HOME` no agent reads, relative to it. A trailing `/` is a directory (all of it),
/// a trailing `*` a name prefix.
pub const PROTECTED: [&str; 14] = [
    "signing/",
    "secrets/",
    "backups/",
    // the upgrade journal: the control token's file, the supervisor's database copies, runner jobs
    "updates/",
    "steve.env*",
    "steve.db*",
    "data/steve.sqlite3*",
    "data/files/",
    // raw mail (steve-mail: `data/mail/<ws>/…/<id>.eml`) and the Hub's state per paired Node
    "data/mail/",
    "data/nodes/",
    "data/node/node.sqlite3*",
    "data/node/secrets/",
    "data/node/push/",
    "data/node/blobs/",
];

/// Environment variables the Hub has and no harness inherits: the upgrade control channel, and what
/// points at the vault's key store or a secret file.
pub fn hub_private_env(key: &str) -> bool {
    matches!(
        key,
        "STEVE_UPGRADE_CONTROL_TOKEN" | "STEVE_SUPERVISOR_PID" | "STEVE_UPGRADE_ID"
    ) || key.starts_with("STEVE_VAULT_")
        || key.starts_with("STEVE_OIDC_")
        || key.starts_with("STEVE_KEYHELPER")
        || key.ends_with("_SECRET_FILE")
        || key.ends_with("_KEY_FILE")
        || key.ends_with("_PASSWORD_FILE")
        || (key.starts_with("STEVE_") && (key.ends_with("_SECRET") || key.ends_with("_PASSWORD")))
}

/// The names of [`hub_private_env`] in this process's environment (to remove from a child's).
pub fn hub_private_vars() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .filter(|(k, _)| k.to_str().is_some_and(hub_private_env))
        .map(|(k, _)| k)
        .collect()
}

/// `STEVE_HOME` as the Hub resolves it (`~/.steve` when unset), absolute.
pub fn steve_home_from_env() -> PathBuf {
    let home = std::env::var_os("STEVE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".steve")
        });
    std::path::absolute(&home).unwrap_or(home)
}

/// `steve_home` with symlinks resolved when it exists (the harnesses match real paths).
fn real(steve_home: &Path) -> PathBuf {
    std::fs::canonicalize(steve_home).unwrap_or_else(|_| steve_home.to_path_buf())
}

/// Claude Code permission rules (`permissions.deny`) for [`PROTECTED`] under `steve_home`. Deny rules
/// hold in every permission mode, `bypassPermissions` included, but only for Claude Code's own file
/// tools (Read, Edit, Write, Grep, Glob …): a program a Bash command runs (`python3 -c`, `sqlite3`,
/// `tar`) is not bound by them. Keeping those out needs an OS sandbox (`sandbox-exec`), not rules.
pub fn claude_deny_rules(steve_home: &Path) -> Vec<String> {
    let mut roots = vec![steve_home.to_path_buf()];
    let r = real(steve_home);
    if r != roots[0] {
        roots.push(r);
    }
    let mut out = Vec::new();
    for root in &roots {
        let root = root.to_string_lossy();
        let root = root.trim_end_matches('/');
        for p in PROTECTED {
            // `//` starts an absolute path in a Claude Code rule
            let pattern = match p.strip_suffix('/') {
                Some(dir) => format!("/{root}/{dir}/**"),
                None => format!("/{root}/{p}"),
            };
            for tool in ["Read", "Edit"] {
                out.push(format!("{tool}({pattern})"));
            }
        }
    }
    out
}

/// The settings object (`--settings`, or the ACP adapter's `_meta.claudeCode.options.settings`).
pub fn claude_settings(steve_home: &Path) -> serde_json::Value {
    serde_json::json!({ "permissions": { "deny": claude_deny_rules(steve_home) } })
}

/// A Codex permission profile entry (`permissions.<name>.filesystem`) for each of [`PROTECTED`]:
/// directories by path, name prefixes as globs. Codex keeps deny entries as deny-read.
pub fn codex_deny_filesystem(steve_home: &Path) -> String {
    let root = real(steve_home);
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    let entries: Vec<String> = PROTECTED
        .iter()
        .map(|p| {
            let path = match p.strip_suffix('/') {
                Some(dir) => format!("{root}/{dir}"),
                None => format!("{root}/{p}"),
            };
            format!("{}=\"deny\"", toml_str(&path))
        })
        .collect();
    format!("{{{}}}", entries.join(","))
}

/// The parts of a Node's own home (its journal, and the secrets, pushes and attachments it keeps
/// for runs) no run reads, relative to it. For the Hub's local Node they are also in [`PROTECTED`].
pub const NODE_PROTECTED: [&str; 6] = [
    "node.sqlite3*",
    "secrets/",
    "push/",
    "blobs/",
    // every principal's own harness directories and caches: a run is given back its own
    "state/",
    "cache/",
];

/// A remote Node's own files, relative to its `$STEVE_HOME` (`~/.steve-node`): its identity, its
/// pairing and its owner's policy, roots and harness settings.
pub const NODE_FILES: [&str; 5] = [
    "node/identity.key",
    "node/node.json",
    "policy.json",
    "roots.json",
    "harness.json",
];

/// Steve's own programs and their builds under `STEVE_HOME`: a run may read them, never change
/// them (a run that could replace the Hub's binary would run as the Hub next time).
pub const SEALED: [&str; 6] = ["releases/", "current", "web/", "bin/", "build/", "plugins/"];

/// One SBPL path filter for `entry` under `root`: a trailing `/` is the directory, a trailing `*`
/// a name prefix (a regex), else the one path. `None`: a path SBPL could not quote safely.
fn sbpl_filter(root: &str, entry: &str) -> Option<Vec<String>> {
    if root
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        return None;
    }
    Some(if let Some(dir) = entry.strip_suffix('/') {
        vec![format!("(subpath \"{root}/{dir}\")")]
    } else if let Some(prefix) = entry.strip_suffix('*') {
        let mut re = String::from("^");
        for c in format!("{root}/{prefix}").chars() {
            if ".^$|?*+()[]{}".contains(c) {
                re.push('\\');
            }
            re.push(c);
        }
        vec![format!("(regex #\"{re}\")")]
    } else {
        // the path itself (a symlink such as `current` is renamed or removed by its own name) and,
        // when it is a directory, what is under it
        vec![
            format!("(literal \"{root}/{entry}\")"),
            format!("(subpath \"{root}/{entry}\")"),
        ]
    })
}

/// `root` as given and, when it differs, with symlinks resolved (the sandbox sees real paths).
fn roots_of(root: &Path) -> Vec<String> {
    let mut out = vec![root.to_string_lossy().trim_end_matches('/').to_string()];
    let r = real(root)
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string();
    if r != out[0] {
        out.push(r);
    }
    out
}

/// Every directory a rule's path goes through, from `/` down to the entry's parent: renaming one
/// would move the entry to a path no rule names (`mv ~/.steve/data d2; cat d2/files/…`).
fn ancestors_of(root: &str, entry: &str, out: &mut std::collections::BTreeSet<String>) {
    let mut p = PathBuf::from(format!("{root}/{}", entry.trim_end_matches(['/', '*'])));
    while let Some(up) = p.parent() {
        out.insert(up.to_string_lossy().into_owned());
        p = up.to_path_buf();
    }
}

/// The control plane as seatbelt (macOS `sandbox-exec`) rules, for any process a Node starts for a
/// run: [`PROTECTED`] and [`NODE_FILES`] under `steve_home` and [`NODE_PROTECTED`] under `node_home`
/// can be neither read nor written, [`SEALED`] under `steve_home` not written, and no directory on
/// the way to them renamed or removed (making files inside those stays open). Seatbelt checks the
/// real path whatever a process names: a symlink, a hard link it would make, another case, `..`.
/// They go after a profile's own rules, so they win over its allows. `None`: a path SBPL cannot
/// quote.
pub fn seatbelt_rules(steve_home: &Path, node_home: &Path) -> Option<String> {
    let mut hidden = Vec::new();
    let mut sealed = Vec::new();
    let mut ways = std::collections::BTreeSet::new();
    for root in roots_of(steve_home) {
        for e in PROTECTED.iter().chain(NODE_FILES.iter()) {
            hidden.extend(sbpl_filter(&root, e)?);
            ancestors_of(&root, e, &mut ways);
        }
        for e in SEALED {
            sealed.extend(sbpl_filter(&root, e)?);
            ancestors_of(&root, e, &mut ways);
        }
    }
    for root in roots_of(node_home) {
        for e in NODE_PROTECTED {
            hidden.extend(sbpl_filter(&root, e)?);
            ancestors_of(&root, e, &mut ways);
        }
    }
    let ways: Vec<String> = ways.iter().map(|w| format!("(literal \"{w}\")")).collect();
    Some(format!(
        "(deny file-read* file-write*\n  {})\n(deny file-write*\n  {})\n(deny file-write*\n  {})\n",
        hidden.join("\n  "),
        sealed.join("\n  "),
        ways.join("\n  ")
    ))
}

/// Whether this process can start a seatbelt sandbox at all: not on another platform, and not
/// from inside one (macOS nests none), as when Steve's own tests run in a confined session.
pub fn seatbelt_works() -> bool {
    cfg!(target_os = "macos")
        && std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// Where runs write under `STEVE_HOME`: the items' worktrees, the scratch directories and the
/// Node's own directories for them (its jobs, its worktrees).
pub const RUN_DIRS: [&str; 3] = ["worktrees", "scratch", "data/node"];

/// The same protection as [`seatbelt_rules`] for bubblewrap (Linux `bwrap`), as an allowlist: all
/// of `steve_home` is covered by an empty tmpfs (so nothing there is readable, including files the
/// Hub writes later), the run directories in it and `node_home` are bound back writable, and inside
/// those [`NODE_PROTECTED`] is hidden again: a directory by its own tmpfs, a file by `/dev/null` (a
/// name prefix matches the files there now; the journal's are made when the Node opens). Real,
/// absolute paths, in mount order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BwrapPlan {
    pub masked: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub hidden_dirs: Vec<PathBuf>,
    pub hidden_files: Vec<PathBuf>,
}

pub fn bwrap_plan(steve_home: &Path, node_home: &Path) -> BwrapPlan {
    let (sh, nh) = (real(steve_home), real(node_home));
    let mut p = BwrapPlan::default();
    if sh.is_dir() {
        p.masked.push(sh.clone());
    }
    for d in RUN_DIRS {
        if sh.join(d).is_dir() {
            p.writable.push(sh.join(d));
        }
    }
    if nh.is_dir() && !p.writable.iter().any(|w| nh.starts_with(w)) {
        p.writable.push(nh.clone());
    }
    for e in NODE_PROTECTED {
        match (e.strip_suffix('/'), e.strip_suffix('*')) {
            (Some(dir), _) => p.hidden_dirs.push(nh.join(dir)),
            (_, Some(prefix)) => {
                if let Ok(rd) = std::fs::read_dir(&nh) {
                    for f in rd.flatten() {
                        if f.file_name().to_string_lossy().starts_with(prefix) {
                            p.hidden_files.push(f.path());
                        }
                    }
                }
            }
            _ => p.hidden_files.push(nh.join(e)),
        }
    }
    p.hidden_files.retain(|f| f.is_file());
    for v in [&mut p.hidden_dirs, &mut p.hidden_files] {
        v.sort();
        v.dedup();
    }
    p
}

/// A TOML basic string.
fn toml_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if c.is_control() => o.push_str(&format!("\\u{:04X}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_private_env_names() {
        for k in [
            "STEVE_UPGRADE_CONTROL_TOKEN",
            "STEVE_SUPERVISOR_PID",
            "STEVE_UPGRADE_ID",
            "STEVE_OIDC_CLIENT_SECRET_FILE",
            "STEVE_OIDC_CLIENT_ID",
            "STEVE_VAULT_KEYSTORE",
            "STEVE_VAULT_KEY_DIR",
            "STEVE_KEYHELPER",
            "OPENVIKING_KEY_FILE",
            "STEVE_SMTP_PASSWORD",
            "STEVE_WEBHOOK_SECRET",
            "MAIL_PASSWORD_FILE",
        ] {
            assert!(hub_private_env(k), "{k}");
        }
        for k in [
            "PATH",
            "HOME",
            "STEVE_HOME",
            "STEVE_PORT",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_EXECUTABLE",
            "CODEX_HOME",
            "SECRET",
        ] {
            assert!(!hub_private_env(k), "{k}");
        }
    }

    #[test]
    fn claude_rules_name_every_protected_path_absolutely() {
        let home = Path::new("/nonexistent-steve-home/.steve");
        let rules = claude_deny_rules(home);
        assert_eq!(rules.len(), PROTECTED.len() * 2);
        assert!(rules.contains(&"Read(//nonexistent-steve-home/.steve/signing/**)".to_string()));
        assert!(rules.contains(&"Read(//nonexistent-steve-home/.steve/steve.env*)".to_string()));
        assert!(rules.contains(&"Edit(//nonexistent-steve-home/.steve/secrets/**)".to_string()));
        assert!(
            rules
                .contains(&"Read(//nonexistent-steve-home/.steve/data/steve.sqlite3*)".to_string())
        );
        // raw mail, the Hub's per-Node state and the upgrade journal (control token, database
        // copies) are the owner's too
        for dir in [
            "data/mail",
            "data/nodes",
            "data/files",
            "data/node/secrets",
            "updates",
        ] {
            let want = format!("Read(//nonexistent-steve-home/.steve/{dir}/**)");
            assert!(rules.contains(&want), "{want}");
        }
        // the agents' own working directories stay readable
        for r in &rules {
            for own in [
                "worktrees",
                "scratch",
                "data/node/jobs",
                "data/node/worktrees",
            ] {
                assert!(!r.contains(own), "{r}");
            }
        }
        let s = claude_settings(home);
        assert_eq!(
            s["permissions"]["deny"].as_array().unwrap().len(),
            rules.len()
        );
    }

    #[test]
    fn claude_rules_also_cover_the_real_path() {
        let tmp = std::env::temp_dir().join(format!("steve-harness-{}", std::process::id()));
        let real_dir = tmp.join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let link = tmp.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();
        let rules = claude_deny_rules(&link);
        let real = std::fs::canonicalize(&real_dir).unwrap();
        assert!(rules.contains(&format!("Read(/{}/signing/**)", link.display())));
        assert!(rules.contains(&format!("Read(/{}/signing/**)", real.display())));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn seatbelt_rules_hide_the_control_plane_and_seal_the_programs() {
        let r = seatbelt_rules(
            Path::new("/nonexistent-steve-home/.steve"),
            Path::new("/nonexistent-node-home"),
        )
        .unwrap();
        let (hidden, rest) = r.split_once("(deny file-write*\n").unwrap();
        let (sealed, ways) = rest.split_once("(deny file-write*\n").unwrap();
        for w in [
            "/",
            "/nonexistent-steve-home",
            "/nonexistent-steve-home/.steve",
        ] {
            assert!(ways.contains(&format!("(literal \"{w}\")")), "{w}");
        }
        assert!(ways.contains(r#"(literal "/nonexistent-steve-home/.steve/data/node")"#));
        assert!(hidden.starts_with("(deny file-read* file-write*"));
        assert!(hidden.contains(r#"(subpath "/nonexistent-steve-home/.steve/signing")"#));
        assert!(hidden.contains(r#"(regex #"^/nonexistent-steve-home/\.steve/steve\.env")"#));
        assert!(hidden.contains(r#"(literal "/nonexistent-steve-home/.steve/node/identity.key")"#));
        assert!(hidden.contains(r#"(subpath "/nonexistent-node-home/secrets")"#));
        assert!(hidden.contains(r#"(regex #"^/nonexistent-node-home/node\.sqlite3")"#));
        assert!(sealed.contains(r#"(subpath "/nonexistent-steve-home/.steve/releases")"#));
        assert!(sealed.contains(r#"(literal "/nonexistent-steve-home/.steve/current")"#));
        for own in ["worktrees", "scratch", "jobs", "codex"] {
            assert!(
                !r.contains(own),
                "{own}: the runs' own directories stay open"
            );
        }
        assert!(seatbelt_rules(Path::new("/a\"b"), Path::new("/n")).is_none());
    }

    /// The rules as a real `sandbox-exec` applies them: the control plane is unreadable and
    /// unwritable, the programs are readable and unwritable, the runs' own directories are open.
    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_rules_hold_under_sandbox_exec() {
        if !seatbelt_works() {
            eprintln!("skipped: no seatbelt here (inside a sandbox already?)");
            return;
        }
        let t = std::env::temp_dir().join(format!("steve-confine-{}", std::process::id()));
        let home = t.join("home");
        let node = home.join("data/node");
        for d in [
            "signing",
            "releases/0.1",
            "worktrees/ST-1",
            "data/node/secrets",
            "data/node/jobs",
        ] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        for f in [
            "signing/k",
            "steve.env",
            "steve.env.bak",
            "releases/0.1/steve-hub",
            "worktrees/ST-1/a",
            "data/node/secrets/s",
            "data/node/node.sqlite3",
        ] {
            std::fs::write(home.join(f), "x").unwrap();
        }
        let profile = format!(
            "(version 1)\n(allow default)\n{}",
            seatbelt_rules(&home, &node).unwrap()
        );
        let sh = |cmd: &str| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile, "/bin/sh", "-c", cmd])
                .current_dir(&t)
                .status()
                .unwrap()
                .success()
        };
        let h = home.display();
        for denied in [
            format!("cat {h}/signing/k"),
            format!("cat {h}/steve.env"),
            format!("cat {h}/steve.env.bak"),
            format!("ls {h}/signing"),
            format!("cat {h}/data/node/secrets/s"),
            format!("cat {h}/data/node/node.sqlite3"),
            format!("echo y > {h}/releases/0.1/steve-hub"),
            format!("echo y > {h}/signing/new"),
            format!("rm {h}/releases/0.1/steve-hub"),
            // other names for the same files
            format!("cat {h}/SIGNING/k"),
            format!("cat {h}/worktrees/../signing/k"),
            format!("ln -s {h}/signing/k {h}/worktrees/ST-1/s && cat {h}/worktrees/ST-1/s"),
            format!("ln {h}/signing/k {h}/worktrees/ST-1/h"),
            // moving a directory on the way out from under the rules
            format!("mv {h}/data {h}/d2"),
            format!("mv {h}/signing {h}/worktrees/s"),
            format!("mv {h} {h}.x"),
        ] {
            assert!(!sh(&denied), "allowed: {denied}");
        }
        for allowed in [
            format!("cat {h}/releases/0.1/steve-hub"),
            format!("cat {h}/worktrees/ST-1/a"),
            format!("echo y > {h}/worktrees/ST-1/b"),
            format!("echo y > {h}/data/node/jobs/j"),
            format!("mkdir {h}/worktrees/ST-2 && mv {h}/worktrees/ST-2 {h}/worktrees/ST-3"),
            format!("echo y > {h}/new-file"),
            format!("mkdir -p {h}/data/node/jobs/x/y"),
        ] {
            assert!(sh(&allowed), "denied: {allowed}");
        }
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn bwrap_plan_masks_the_home_and_opens_the_run_dirs() {
        let t = std::env::temp_dir().join(format!("steve-bwrap-{}", std::process::id()));
        let home = t.join("home");
        for d in [
            "signing",
            "worktrees",
            "data/node/jobs",
            "data/node/secrets",
        ] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::write(home.join("steve.env"), "").unwrap();
        std::fs::write(home.join("data/node/node.sqlite3"), "").unwrap();
        std::fs::write(home.join("data/node/node.sqlite3-wal"), "").unwrap();
        let p = bwrap_plan(&home, &home.join("data/node"));
        let real = std::fs::canonicalize(&home).unwrap();
        assert_eq!(
            p.masked,
            vec![real.clone()],
            "all of it, files made later too"
        );
        assert_eq!(
            p.writable,
            vec![real.join("worktrees"), real.join("data/node")]
        );
        for d in ["data/node/secrets", "data/node/push", "data/node/blobs"] {
            assert!(p.hidden_dirs.contains(&real.join(d)), "{d}");
        }
        assert_eq!(
            p.hidden_files,
            vec![
                real.join("data/node/node.sqlite3"),
                real.join("data/node/node.sqlite3-wal")
            ]
        );
        // a remote Node: its own home masked, only its Node directory bound back
        let remote = t.join("steve-node");
        std::fs::create_dir_all(remote.join("node/local")).unwrap();
        let p = bwrap_plan(&remote, &remote.join("node/local"));
        let r = std::fs::canonicalize(&remote).unwrap();
        assert_eq!(p.masked, vec![r.clone()]);
        assert_eq!(p.writable, vec![r.join("node/local")]);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn codex_filesystem_is_a_toml_inline_table() {
        let fs = codex_deny_filesystem(Path::new("/nonexistent-steve-home/.st\"eve"));
        assert!(fs.starts_with('{') && fs.ends_with('}'), "{fs}");
        assert!(
            fs.contains(r#""/nonexistent-steve-home/.st\"eve/signing"="deny""#),
            "{fs}"
        );
        assert!(
            fs.contains(r#""/nonexistent-steve-home/.st\"eve/steve.env*"="deny""#),
            "{fs}"
        );
        assert!(
            fs.contains(r#""/nonexistent-steve-home/.st\"eve/updates"="deny""#),
            "{fs}"
        );
        assert_eq!(fs.matches("=\"deny\"").count(), PROTECTED.len());
    }
}
