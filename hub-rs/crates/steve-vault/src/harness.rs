//! Harness secret filtering and harness-native permission rules. OS sandbox policy and
//! probing are owned by steve-confine, shared with Node and Research.
use std::collections::HashSet;
use std::path::Path;
use steve_confine::{NODE_FILES, NODE_PROTECTED, PROTECTED, SEALED, canonical_path};

/// Antigravity CLI permission allow rule covering every tool exposed by Steve's `"steve"` MCP server.
pub const STEVE_MCP_ALLOW_RULE: &str = "mcp(steve/*)";
/// `NODE_PROTECTED` subdirectories left open in Antigravity deny rules so each session keeps access
/// to its own isolated harness state and toolchain caches (Antigravity `deny` always beats `allow`).
pub const ANTIGRAVITY_OPEN_NODE_DIRS: [&str; 2] = ["state/", "cache/"];
/// Companion files created beside SQLite databases (`data/steve.sqlite3*`, `node.sqlite3*`).
const SQLITE_COMPANION_SUFFIXES: [&str; 4] = ["", "-wal", "-shm", "-journal"];
/// Backup/staging variants commonly placed beside `steve.env*`.
const ENV_COMPANION_SUFFIXES: [&str; 4] = ["", ".bak", ".tmp", ".new"];

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

/// Claude Code permission rules (`permissions.deny`) for [`PROTECTED`] under `steve_home`. Deny rules
/// hold in every permission mode, `bypassPermissions` included, but only for Claude Code's own file
/// tools (Read, Edit, Write, Grep, Glob …): a program a Bash command runs (`python3 -c`, `sqlite3`,
/// `tar`) is not bound by them. Keeping those out needs an OS sandbox (`sandbox-exec`), not rules.
pub fn claude_deny_rules(steve_home: &Path) -> Vec<String> {
    let mut roots = vec![steve_home.to_path_buf()];
    let r = steve_confine::canonical_path(steve_home);
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

/// Antigravity CLI `permissions.allow` rules for Steve's own MCP endpoint (`mcp(steve/*)`).
/// Individual Steve tool permissions remain enforced on the Hub side.
pub fn antigravity_allow_rules() -> Vec<String> {
    vec![STEVE_MCP_ALLOW_RULE.to_string()]
}

/// Antigravity CLI `read_file`/`write_file` deny rules for one concrete path (`None` if empty or if
/// containing `)`, glob metacharacters `*`/`?`/`[`/`]`, or control characters; `agy`'s terminal
/// sandbox rejects glob deny paths with `sandbox configuration error: ... globs not supported`).
pub fn antigravity_path_deny_rules(path: &Path) -> Option<[String; 2]> {
    let raw = path.to_string_lossy();
    let trimmed = if raw == "/" {
        "/"
    } else {
        raw.trim_end_matches('/')
    };
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|c| matches!(c, ')' | '*' | '?' | '[' | ']') || c.is_control())
    {
        return None;
    }
    Some([
        format!("read_file({trimmed})"),
        format!("write_file({trimmed})"),
    ])
}

fn normalized_roots(path: &Path) -> Vec<String> {
    let normalize = |p: &Path| {
        let s = p.to_string_lossy();
        if s == "/" {
            "/".to_owned()
        } else {
            s.trim_end_matches('/').to_owned()
        }
    };
    let mut out = vec![normalize(path)];
    let real = normalize(&canonical_path(path));
    if real != out[0] {
        out.push(real);
    }
    out
}

fn join_root(root: &str, rel: &str) -> String {
    if root == "/" {
        format!("/{rel}")
    } else {
        format!("{root}/{rel}")
    }
}

fn expand_antigravity_entry(root: &str, entry: &str) -> Vec<String> {
    if let Some(dir) = entry.strip_suffix('/') {
        vec![join_root(root, dir)]
    } else if let Some(prefix) = entry.strip_suffix('*') {
        if prefix.ends_with(".sqlite3") || prefix.ends_with(".db") {
            SQLITE_COMPANION_SUFFIXES
                .iter()
                .map(|sfx| format!("{}{sfx}", join_root(root, prefix)))
                .collect()
        } else if prefix.ends_with(".env") {
            ENV_COMPANION_SUFFIXES
                .iter()
                .map(|sfx| format!("{}{sfx}", join_root(root, prefix)))
                .collect()
        } else {
            vec![join_root(root, prefix)]
        }
    } else {
        vec![join_root(root, entry)]
    }
}

/// Antigravity CLI `permissions.deny` rules (`read_file(...)` / `write_file(...)`) protecting
/// [`PROTECTED`], [`NODE_FILES`], [`SEALED`] (write-only), and [`NODE_PROTECTED`] (excluding
/// `state/` and `cache/` so each session keeps access to its own isolated directory and caches).
pub fn antigravity_deny_rules(steve_home: &Path, node_home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push_unique = |rule: String| {
        if seen.insert(rule.clone()) {
            out.push(rule);
        }
    };

    for root in normalized_roots(steve_home) {
        for entry in PROTECTED.iter().chain(NODE_FILES.iter()) {
            for target in expand_antigravity_entry(&root, entry) {
                if let Some([read, write]) = antigravity_path_deny_rules(Path::new(&target)) {
                    push_unique(read);
                    push_unique(write);
                }
            }
        }
        for entry in SEALED {
            for target in expand_antigravity_entry(&root, entry) {
                if let Some([_, write]) = antigravity_path_deny_rules(Path::new(&target)) {
                    push_unique(write);
                }
            }
        }
    }

    for root in normalized_roots(node_home) {
        for entry in NODE_PROTECTED {
            if ANTIGRAVITY_OPEN_NODE_DIRS.contains(&entry) {
                continue;
            }
            for target in expand_antigravity_entry(&root, entry) {
                if let Some([read, write]) = antigravity_path_deny_rules(Path::new(&target)) {
                    push_unique(read);
                    push_unique(write);
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_confine::{bwrap_plan, seatbelt_rules, seatbelt_works};

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
    fn antigravity_rules_allow_steve_mcp_and_deny_protected_paths() {
        assert_eq!(antigravity_allow_rules(), vec!["mcp(steve/*)".to_string()]);
        let steve_home = Path::new("/nonexistent-steve-home/.steve");
        let default_node = steve_home.join("data/node");
        let rules = antigravity_deny_rules(steve_home, &default_node);

        // Deduped when node_home is steve_home/data/node.
        let unique: HashSet<&String> = rules.iter().collect();
        assert_eq!(unique.len(), rules.len(), "rules must be deduplicated");

        // No Claude-style `//` prefixes or `/**` globs or raw `*`.
        for r in &rules {
            assert!(!r.contains("/**"), "unexpected glob suffix: {r}");
            assert!(!r.contains('*'), "unexpected wildcard: {r}");
            assert!(!r.contains("(//"), "unexpected claude double-slash: {r}");
        }

        // Protected directories & files get both read_file and write_file.
        for path in [
            "/nonexistent-steve-home/.steve/signing",
            "/nonexistent-steve-home/.steve/secrets",
            "/nonexistent-steve-home/.steve/backups",
            "/nonexistent-steve-home/.steve/updates",
            "/nonexistent-steve-home/.steve/steve.env",
            "/nonexistent-steve-home/.steve/steve.env.bak",
            "/nonexistent-steve-home/.steve/steve.db",
            "/nonexistent-steve-home/.steve/steve.db-wal",
            "/nonexistent-steve-home/.steve/data/steve.sqlite3",
            "/nonexistent-steve-home/.steve/data/steve.sqlite3-wal",
            "/nonexistent-steve-home/.steve/data/steve.sqlite3-shm",
            "/nonexistent-steve-home/.steve/data/files",
            "/nonexistent-steve-home/.steve/data/mail",
            "/nonexistent-steve-home/.steve/data/nodes",
            "/nonexistent-steve-home/.steve/data/node/node.sqlite3",
            "/nonexistent-steve-home/.steve/data/node/node.sqlite3-wal",
            "/nonexistent-steve-home/.steve/data/node/secrets",
            "/nonexistent-steve-home/.steve/data/node/push",
            "/nonexistent-steve-home/.steve/data/node/blobs",
            "/nonexistent-steve-home/.steve/node/identity.key",
            "/nonexistent-steve-home/.steve/node/node.json",
            "/nonexistent-steve-home/.steve/policy.json",
            "/nonexistent-steve-home/.steve/roots.json",
            "/nonexistent-steve-home/.steve/harness.json",
        ] {
            assert!(
                rules.contains(&format!("read_file({path})")),
                "missing read_file({path})"
            );
            assert!(
                rules.contains(&format!("write_file({path})")),
                "missing write_file({path})"
            );
        }

        // Sealed programs get write_file deny only (readable, not writable).
        for sealed in ["releases", "current", "web", "bin", "build", "plugins"] {
            let p = format!("/nonexistent-steve-home/.steve/{sealed}");
            assert!(
                rules.contains(&format!("write_file({p})")),
                "missing write_file({p})"
            );
            assert!(
                !rules.contains(&format!("read_file({p})")),
                "unexpected read_file({p})"
            );
        }

        // Run dirs and isolated state/cache dirs stay open.
        for r in &rules {
            for own in [
                "worktrees",
                "workspaces",
                "scratch",
                "data/node/state",
                "data/node/cache",
                "data/node/jobs",
                "data/node/worktrees",
            ] {
                assert!(!r.contains(own), "rule {r} must not cover {own}");
            }
        }

        // Separate remote node_home protects its own journal/secrets/push/blobs without blocking state/cache.
        let remote_node = Path::new("/nonexistent-remote-node/node");
        let remote_rules = antigravity_deny_rules(steve_home, remote_node);
        for p in [
            "/nonexistent-remote-node/node/node.sqlite3",
            "/nonexistent-remote-node/node/node.sqlite3-wal",
            "/nonexistent-remote-node/node/secrets",
            "/nonexistent-remote-node/node/push",
            "/nonexistent-remote-node/node/blobs",
        ] {
            assert!(
                remote_rules.contains(&format!("read_file({p})")),
                "missing remote read_file({p})"
            );
            assert!(
                remote_rules.contains(&format!("write_file({p})")),
                "missing remote write_file({p})"
            );
        }
        for open in ["state", "cache"] {
            let p = format!("/nonexistent-remote-node/node/{open}");
            assert!(!remote_rules.contains(&format!("read_file({p})")));
            assert!(!remote_rules.contains(&format!("write_file({p})")));
        }
    }

    #[test]
    fn antigravity_rules_also_cover_canonical_symlink_path() {
        let tmp = std::env::temp_dir().join(format!("steve-agy-vault-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let real_home = tmp.join("real-steve");
        let real_node = tmp.join("real-node");
        std::fs::create_dir_all(&real_home).unwrap();
        std::fs::create_dir_all(&real_node).unwrap();
        let link_home = tmp.join("link-steve");
        let link_node = tmp.join("link-node");
        std::os::unix::fs::symlink(&real_home, &link_home).unwrap();
        std::os::unix::fs::symlink(&real_node, &link_node).unwrap();

        let rules = antigravity_deny_rules(&link_home, &link_node);
        let canon_home = std::fs::canonicalize(&real_home).unwrap();
        let canon_node = std::fs::canonicalize(&real_node).unwrap();
        assert!(rules.contains(&format!("read_file({}/signing)", link_home.display())));
        assert!(rules.contains(&format!("read_file({}/signing)", canon_home.display())));
        assert!(rules.contains(&format!("write_file({}/secrets)", link_node.display())));
        assert!(rules.contains(&format!("write_file({}/secrets)", canon_node.display())));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn antigravity_path_deny_rules_rejects_unsafe_delimiters() {
        assert_eq!(antigravity_path_deny_rules(Path::new("")), None);
        assert_eq!(antigravity_path_deny_rules(Path::new("/a)b")), None);
        assert_eq!(antigravity_path_deny_rules(Path::new("/a\nb")), None);
        assert_eq!(
            antigravity_path_deny_rules(Path::new("/tmp/steve.env*")),
            None
        );
        assert_eq!(
            antigravity_path_deny_rules(Path::new("/tmp/steve.env?")),
            None
        );
        assert_eq!(antigravity_path_deny_rules(Path::new("/tmp/[a]")), None);
        assert_eq!(antigravity_path_deny_rules(Path::new("/tmp/a]b")), None);
        assert_eq!(
            antigravity_path_deny_rules(Path::new("/tmp/foo/")),
            Some([
                "read_file(/tmp/foo)".to_string(),
                "write_file(/tmp/foo)".to_string()
            ])
        );
    }
}
