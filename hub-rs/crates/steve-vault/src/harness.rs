//! Harness secret filtering and harness-native permission rules. OS sandbox policy and
//! probing are owned by steve-confine, shared with Node and Research.
use std::path::{Path, PathBuf};
pub use steve_confine::{
    BwrapPlan, NODE_FILES, NODE_PROTECTED, PROTECTED, RUN_DIRS, SEALED, bwrap_path, bwrap_plan,
    bwrap_works, seatbelt_rules, seatbelt_works,
};

/// Environment variables the Hub has and no harness inherits: the upgrade control channel, and what
/// points at the vault's key store or a secret file.
pub fn hub_private_env(key: &str) -> bool {
    matches!(
        key,
        "STEVE_UPGRADE_CONTROL_TOKEN"
            | "STEVE_SUPERVISOR_PID"
            | "STEVE_UPGRADE_ID"
            | "OPENVIKING_KEY_FILE"
            | "MAIL_PASSWORD_FILE"
    ) || key.starts_with("STEVE_VAULT_")
        || key.starts_with("STEVE_OIDC_")
        || key.starts_with("STEVE_KEYHELPER")
        || (key.starts_with("STEVE_")
            && (key.ends_with("_SECRET")
                || key.ends_with("_PASSWORD")
                || key.ends_with("_SECRET_FILE")
                || key.ends_with("_KEY_FILE")
                || key.ends_with("_PASSWORD_FILE")))
}

/// The names of [`hub_private_env`] in this process's environment (to remove from a child's).
pub fn hub_private_vars() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .filter(|(k, _)| k.to_str().is_some_and(hub_private_env))
        .map(|(k, _)| k)
        .collect()
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
}
