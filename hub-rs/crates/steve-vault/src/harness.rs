//! What an agent harness Steve launches (Claude Code, Codex) must not see of the Hub: the files that
//! unlock the vault or hold the Hub's secrets, and the environment that points at them.
//!
//! Agents run as the owner's user, so these files are readable to them unless the harness itself
//! refuses. Not all of `STEVE_HOME`: item worktrees and the Node's scratch and job directories live
//! there and are the agents' own working directories.

use std::path::{Path, PathBuf};

/// The parts of `STEVE_HOME` no agent reads, relative to it. A trailing `/` is a directory (all of it),
/// a trailing `*` a name prefix.
pub const PROTECTED: [&str; 13] = [
    "signing/",
    "secrets/",
    "backups/",
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
        // raw mail and the Hub's per-Node state are the owner's too
        for dir in ["data/mail", "data/nodes", "data/files", "data/node/secrets"] {
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
        assert_eq!(fs.matches("=\"deny\"").count(), PROTECTED.len());
    }
}
