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
pub const NODE_PROTECTED: [&str; 4] = ["node.sqlite3*", "secrets/", "push/", "blobs/"];

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

/// The control plane as seatbelt (macOS `sandbox-exec`) rules, for any process a Node starts for a
/// run: [`PROTECTED`] and [`NODE_FILES`] under `steve_home` and [`NODE_PROTECTED`] under `node_home`
/// can be neither read nor written, and [`SEALED`] under `steve_home` not written. They go after a
/// profile's own rules, so they win over its allows. `None`: a path SBPL cannot quote.
pub fn seatbelt_rules(steve_home: &Path, node_home: &Path) -> Option<String> {
    let mut hidden = Vec::new();
    let mut sealed = Vec::new();
    for root in roots_of(steve_home) {
        for e in PROTECTED.iter().chain(NODE_FILES.iter()) {
            hidden.extend(sbpl_filter(&root, e)?);
        }
        for e in SEALED {
            sealed.extend(sbpl_filter(&root, e)?);
        }
    }
    for root in roots_of(node_home) {
        for e in NODE_PROTECTED {
            hidden.extend(sbpl_filter(&root, e)?);
        }
    }
    Some(format!(
        "(deny file-read* file-write*\n  {})\n(deny file-write*\n  {})\n",
        hidden.join("\n  "),
        sealed.join("\n  ")
    ))
}

/// The paths bubblewrap (Linux `bwrap`) hides or seals for the same rules as [`seatbelt_rules`]: a
/// name prefix is expanded to what exists now. `(hidden, sealed)`, each existing and absolute.
pub fn bwrap_paths(steve_home: &Path, node_home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    fn expand(root: &Path, entry: &str, out: &mut Vec<PathBuf>) {
        let entry = entry.trim_end_matches('/');
        match entry.strip_suffix('*') {
            Some(prefix) => {
                let (dir, name) = match prefix.rsplit_once('/') {
                    Some((d, n)) => (root.join(d), n.to_string()),
                    None => (root.to_path_buf(), prefix.to_string()),
                };
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        if e.file_name().to_string_lossy().starts_with(&name) {
                            out.push(e.path());
                        }
                    }
                }
            }
            None => {
                let p = root.join(entry);
                if std::fs::symlink_metadata(&p).is_ok() {
                    out.push(p);
                }
            }
        }
    }
    let (sh, nh) = (real(steve_home), real(node_home));
    let (mut hidden, mut sealed) = (Vec::new(), Vec::new());
    for e in PROTECTED.iter().chain(NODE_FILES.iter()) {
        expand(&sh, e, &mut hidden);
    }
    for e in NODE_PROTECTED {
        expand(&nh, e, &mut hidden);
    }
    for e in SEALED {
        expand(&sh, e, &mut sealed);
    }
    hidden.sort();
    hidden.dedup();
    sealed.sort();
    sealed.dedup();
    (hidden, sealed)
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
        let (hidden, sealed) = r.split_once("(deny file-write*\n").unwrap();
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
        ] {
            assert!(!sh(&denied), "allowed: {denied}");
        }
        for allowed in [
            format!("cat {h}/releases/0.1/steve-hub"),
            format!("cat {h}/worktrees/ST-1/a"),
            format!("echo y > {h}/worktrees/ST-1/b"),
            format!("echo y > {h}/data/node/jobs/j"),
        ] {
            assert!(sh(&allowed), "denied: {allowed}");
        }
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn bwrap_paths_are_what_exists() {
        let t = std::env::temp_dir().join(format!("steve-bwrap-{}", std::process::id()));
        let home = t.join("home");
        std::fs::create_dir_all(home.join("signing")).unwrap();
        std::fs::create_dir_all(home.join("releases")).unwrap();
        std::fs::write(home.join("steve.env"), "").unwrap();
        std::fs::write(home.join("steve.env.bak"), "").unwrap();
        let (hidden, sealed) = bwrap_paths(&home, &home.join("data/node"));
        let real = std::fs::canonicalize(&home).unwrap();
        assert_eq!(
            hidden,
            vec![
                real.join("signing"),
                real.join("steve.env"),
                real.join("steve.env.bak")
            ]
        );
        assert_eq!(sealed, vec![real.join("releases")]);
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
