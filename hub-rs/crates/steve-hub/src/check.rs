//! `steve-hub --check-config`: the checks a real start makes on its environment before it opens the
//! database: the configuration (listen addresses and the `STEVE_ALLOW_ANY_HOST` rule included), the
//! local Node mode, the vault's key store, and that every `STEVE_HOST` is an address of this
//! machine. It neither opens the database nor binds the Hub's port (an address is tried on an
//! ephemeral port), so it can run while the old version still serves.
//!
//! ops/local-release.py (and with it the one-click runner) and the upgrade supervisor run a
//! candidate's binary with it, in the environment the candidate would start with, before they drain
//! anything: a `steve.env` the new Hub would refuse stops the deploy while the old version serves.

use std::sync::Arc;

use serde::Serialize;

use crate::config::Config;

/// Typed harness, Node.js, and tailcat availability summary for `--check-config` and upgrade preflight.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct HarnessCheck {
    pub missing_adapters: Vec<String>,
    pub missing_node: bool,
    pub missing_tailcat: bool,
    pub resolved_adapters: Vec<String>,
}

/// What `--check-config` prints (one JSON line); `ok` is false when any error was found.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Report {
    pub ok: bool,
    /// Each names the variable and what to set instead.
    pub errors: Vec<String>,
    /// `http://host:port` for every listen host, when the configuration parsed.
    pub listen: Vec<String>,
    /// Non-blocking setup or migration work, and unavailable optional confinement.
    pub warnings: Vec<String>,
    /// Typed adapter/Node.js availability under the caller's `PATH`.
    pub harnesses: HarnessCheck,
}

/// Every environment check of a real start, on `get` (the process environment in production).
pub fn check(get: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Report {
    let vars: crate::wire::Vars = Arc::new(get);
    let mut errors = Vec::new();
    let cfg = match Config::from_vars(|k| vars(k)) {
        Ok(cfg) => cfg,
        Err(e) => {
            return Report {
                ok: false,
                errors: vec![e.to_string()],
                listen: Vec::new(),
                warnings: Vec::new(),
                harnesses: HarnessCheck::default(),
            };
        }
    };
    for d in [
        &cfg.data_dir,
        &cfg.worktrees,
        &cfg.scratch,
        &cfg.home.join("updates"),
    ] {
        if let Err(e) = creatable(d) {
            errors.push(format!("STEVE_HOME: {e}"));
        }
    }
    if let Some(crate::metrics::Listen::Unix(path)) = &cfg.metrics {
        let parent = path.parent().expect("absolute socket path");
        if !parent.is_dir() {
            errors.push("STEVE_METRICS_LISTEN: unix socket parent directory must exist".into());
        } else if let Err(e) = creatable(parent) {
            errors.push(format!("STEVE_METRICS_LISTEN: {e}"));
        }
    }
    let mut warnings = Vec::new();
    let mut harnesses = HarnessCheck::default();
    match crate::wire::node_mode(&vars, cfg.diagnostic()) {
        Err(e) => errors.push(e.to_string()),
        Ok(None) => {}
        // the local Node's runs in the OS sandbox (never a reason not to start)
        Ok(_) => {
            let (node_warnings, checked) = local_node_checks(&cfg, &vars);
            warnings.extend(node_warnings);
            harnesses = checked;
            if let Err(why) = steve_confine::confinement(
                &cfg.home,
                &cfg.data_dir.join("node"),
                vars("STEVE_CONFINE"),
            ) {
                warnings.push(steve_i18n::t!("hub.check.unconfined", why = why));
            }
        }
    }
    check_tailcat(&cfg, &vars, &mut warnings, &mut harnesses);
    if let Err(e) = crate::nodes::Ingress::from_vars(|k| vars(k)) {
        errors.push(e);
    }
    if let Err(e) = steve_modules::vault::keystore::from_env(&cfg.home, |k| vars(k)) {
        errors.push(format!("vault key store (STEVE_VAULT_KEYSTORE): {e}"));
    }
    if let Err(e) = steve_auth::AuthConfig::from_vars(|k| vars(k)) {
        errors.push(format!(
            "auth config (STEVE_OIDC_CLIENT_SECRET_FILE / STEVE_OIDC_*): {e}"
        ));
    }
    if let Err(e) = steve_mail::MailConfig::from_env(|k| vars(k)) {
        errors.push(format!("mail config: {e}"));
    }
    for h in &cfg.listen.hosts {
        if let Err(e) = std::net::TcpListener::bind((h.as_str(), 0)) {
            errors.push(format!(
                "STEVE_HOST: cannot listen on {h} ({e}); it must be an address of this machine"
            ));
        }
    }
    Report {
        ok: errors.is_empty(),
        errors,
        listen: cfg.listen.urls().split(", ").map(str::to_string).collect(),
        warnings,
        harnesses,
    }
}

/// Setup guidance shared by preflight and a normal boot. No harness is spawned and no login
/// contents are inspected: the operator, not Steve, authenticates each principal's harness.
pub(crate) fn local_node_warnings(cfg: &Config, vars: &crate::wire::Vars) -> Vec<String> {
    local_node_checks(cfg, vars).0
}

fn is_usable_file(p: &std::path::Path, executable: bool) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && (!executable || m.permissions().mode() & 0o111 != 0))
    }
    #[cfg(not(unix))]
    {
        let _ = executable;
        p.is_file()
    }
}

fn resolve_program(program: &str, executable: bool, path: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(program);
    if p.components().count() > 1 {
        is_usable_file(p, executable).then(|| p.to_path_buf())
    } else {
        std::env::split_paths(path)
            .map(|d| d.join(p))
            .find(|candidate| is_usable_file(candidate, executable))
    }
}

fn program_available(program: &str, executable: bool, path: &str) -> bool {
    resolve_program(program, executable, path).is_some()
}

fn read_text(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn shebang_runs_node(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("#!") else {
        return false;
    };
    let mut parts = rest.trim().split_whitespace();
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(bin) = std::path::Path::new(first)
        .file_name()
        .and_then(|s| s.to_str())
    else {
        return false;
    };
    let cmd = if bin == "env" {
        let Some(c) = parts.find(|tok| !tok.starts_with('-') && !tok.contains('=')) else {
            return false;
        };
        c
    } else {
        first
    };
    std::path::Path::new(cmd)
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| matches!(name, "node" | "nodejs"))
}

fn is_node_script(program: &str, path_env: &str) -> bool {
    let resolved = resolve_program(program, false, path_env)
        .unwrap_or_else(|| std::path::PathBuf::from(program));
    let ext = resolved.extension().and_then(|e| e.to_str());
    if ext.is_some_and(|e| matches!(e, "js" | "mjs" | "cjs")) {
        return true;
    }
    if ext.is_some_and(|e| {
        matches!(
            e,
            "py" | "sh" | "bash" | "zsh" | "rb" | "pl" | "exe" | "cmd" | "bat" | "ps1"
        )
    }) {
        return false;
    }
    if resolved.is_file()
        && let Ok(f) = std::fs::File::open(&resolved)
    {
        use std::io::Read;
        let mut head = [0u8; 128];
        if let Ok(n) = f.take(128).read(&mut head)
            && head[..n].starts_with(b"#!")
        {
            let first = head[..n]
                .split(|b| *b == b'\n' || *b == b'\r')
                .next()
                .unwrap_or_default();
            return shebang_runs_node(&String::from_utf8_lossy(first));
        }
    }
    false
}

fn codex_native_pkg() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("@agentclientprotocol/codex-acp-darwin-arm64"),
        ("macos", "x86_64") => Some("@agentclientprotocol/codex-acp-darwin-x64"),
        ("linux", "aarch64") => Some("@agentclientprotocol/codex-acp-linux-arm64"),
        ("linux", "x86_64") => Some("@agentclientprotocol/codex-acp-linux-x64"),
        ("windows", "aarch64") => Some("@agentclientprotocol/codex-acp-win32-arm64"),
        ("windows", "x86_64") => Some("@agentclientprotocol/codex-acp-win32-x64"),
        _ => None,
    }
}

fn resolve_npm_bin(package: &str, bin: &str, root: &std::path::Path) -> std::path::PathBuf {
    let pkg_dir = root.join(package);
    if let Some(raw) = read_text(&pkg_dir.join("package.json"))
        && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        let entry = match manifest.get("bin") {
            Some(serde_json::Value::String(s)) => Some(s.as_str()),
            Some(serde_json::Value::Object(map)) => map
                .get(bin)
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    (map.len() == 1)
                        .then(|| map.values().next()?.as_str())
                        .flatten()
                }),
            _ => None,
        };
        if let Some(rel) = entry
            .map(|s| s.trim_start_matches("./"))
            .filter(|s| !s.is_empty())
        {
            let candidate = pkg_dir.join(rel);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    #[cfg(windows)]
    let name = format!("{bin}.cmd");
    #[cfg(not(windows))]
    let name = bin;
    let local_bin = root.join(".bin").join(name);
    if local_bin.exists() {
        return local_bin;
    }
    if root.parent().and_then(|p| p.file_name()) != Some(std::ffi::OsStr::new("adapters"))
        && let Some(parent) = root.parent()
    {
        #[cfg(windows)]
        let global_bin = parent.join(name);
        #[cfg(not(windows))]
        let global_bin = parent.parent().unwrap_or(parent).join("bin").join(name);
        if global_bin.exists() {
            return global_bin;
        }
    }
    local_bin
}

fn resolve_default_in_root(
    cmd: &steve_harness_spec::DefaultCommand,
    root: &std::path::Path,
) -> std::path::PathBuf {
    match cmd {
        steve_harness_spec::DefaultCommand::Claude => {
            root.join("@agentclientprotocol/claude-agent-acp/dist/index.js")
        }
        steve_harness_spec::DefaultCommand::Codex => {
            if let Some(pkg) = codex_native_pkg() {
                #[cfg(windows)]
                let bin = "bin/codex-acp.exe";
                #[cfg(not(windows))]
                let bin = "bin/codex-acp";
                let native = root.join(pkg).join(bin);
                if native.is_file() {
                    return native;
                }
                let nested = root
                    .join("@agentclientprotocol/codex-acp/node_modules")
                    .join(pkg)
                    .join(bin);
                if nested.is_file() {
                    return nested;
                }
            }
            root.join("@agentclientprotocol/codex-acp/dist/index.js")
        }
        steve_harness_spec::DefaultCommand::NpmEntry {
            package,
            entry,
            windows_entry,
        } => {
            let entry = if cfg!(windows) {
                windows_entry.as_deref().unwrap_or(entry)
            } else {
                entry
            };
            root.join(package).join(entry)
        }
        steve_harness_spec::DefaultCommand::NpmBin { package, bin } => {
            resolve_npm_bin(package, bin, root)
        }
        steve_harness_spec::DefaultCommand::Path { bin } => std::path::PathBuf::from(bin),
    }
}

/// The Steve home whose managed adapters `command` points into (`<home>/adapters/node_modules/…`).
fn managed_home_of(command: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut p = command;
    while let Some(parent) = p.parent() {
        if p.file_name().is_some_and(|n| n == "node_modules")
            && parent.file_name().is_some_and(|n| n == "adapters")
        {
            return parent.parent().map(std::path::Path::to_path_buf);
        }
        p = parent;
    }
    None
}

fn managed_wanted(home: &std::path::Path, id: &str) -> bool {
    let Some(pkg) = steve_harness_spec::builtin(id).and_then(|b| b.npm_package) else {
        return false;
    };
    let manifest = home
        .join("adapters/node_modules")
        .join(pkg)
        .join("package.json");
    let installed = read_text(&manifest)
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|v| {
            v.get("version")
                .and_then(serde_json::Value::as_str)
                .is_some()
        });
    if installed {
        return true;
    }
    let selection: Option<Vec<String>> = read_text(&home.join("adapters/selected.json"))
        .and_then(|raw| serde_json::from_str(&raw).ok());
    match selection {
        Some(chosen) => chosen.iter().any(|c| c == id),
        None => steve_harness_spec::DEFAULT_MANAGED_ADAPTERS.contains(&id),
    }
}

fn global_npm_root_from_node(node: &str, path: &str) -> Option<std::path::PathBuf> {
    let node_path = resolve_program(node, true, path)?;
    let from_bin = |p: &std::path::Path| {
        let bin = p.parent()?;
        let prefix = bin.parent()?;
        let lib = prefix.join("lib/node_modules");
        if lib.is_dir() {
            return Some(lib);
        }
        #[cfg(windows)]
        {
            let win = bin.join("node_modules");
            if win.is_dir() {
                return Some(win);
            }
        }
        None
    };
    from_bin(&node_path).or_else(|| {
        std::fs::canonicalize(&node_path)
            .ok()
            .and_then(|c| from_bin(&c))
    })
}

/// Resolves the adapter commands that the colocated `steve-node --local-node` process will use
/// under `cfg` and `vars`, without depending on `steve-acp` or spawning `npm`.
fn resolved_local_adapters(
    cfg: &Config,
    vars: &crate::wire::Vars,
    node: &str,
    path: &str,
) -> std::collections::BTreeMap<String, String> {
    let managed_root = cfg.home.join("adapters/node_modules");
    let mut global_root: Option<Option<std::path::PathBuf>> = None;
    let mut adapters = std::collections::BTreeMap::new();
    for b in steve_harness_spec::builtins() {
        let cmd = if let Some(explicit) = vars(b.command_var).filter(|v| !v.is_empty()) {
            explicit
        } else if let Some(manifest_cmd) = cfg.manifest_commands.get(b.id) {
            manifest_cmd.to_string_lossy().into_owned()
        } else if steve_harness_spec::is_npm_managed(b.id) {
            let own = resolve_default_in_root(&b.command, &managed_root);
            if own.exists() {
                own.to_string_lossy().into_owned()
            } else if let Some(on_path) = resolve_program(b.cli_bin, true, path) {
                on_path.to_string_lossy().into_owned()
            } else {
                let global = global_root
                    .get_or_insert_with(|| global_npm_root_from_node(node, path))
                    .as_deref()
                    .map(|root| resolve_default_in_root(&b.command, root))
                    .filter(|c| c.exists());
                global.unwrap_or(own).to_string_lossy().into_owned()
            }
        } else {
            resolve_program(b.cli_bin, true, path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| b.cli_bin.to_owned())
        };
        adapters.insert(b.id.to_owned(), cmd);
    }
    for (id, cmd) in &cfg.manifest_commands {
        adapters
            .entry(id.clone())
            .or_insert_with(|| cmd.to_string_lossy().into_owned());
    }
    adapters
}

fn check_tailcat(
    cfg: &Config,
    vars: &crate::wire::Vars,
    warnings: &mut Vec<String>,
    harnesses: &mut HarnessCheck,
) {
    let explicit = vars("STEVE_TAILCAT").filter(|v| !v.trim().is_empty());
    let configured = explicit.is_some() || cfg.home.join("tailcat/hub.private.json").is_file();
    if !configured {
        return;
    }
    let program = explicit.as_deref().unwrap_or("tailcat");
    let path = vars("PATH").unwrap_or_default();
    if !program_available(program, true, &path) {
        harnesses.missing_tailcat = true;
        warnings.push(steve_i18n::t!(
            "hub.check.tailcat_missing",
            tailcat = program
        ));
    }
}

fn local_node_checks(cfg: &Config, vars: &crate::wire::Vars) -> (Vec<String>, HarnessCheck) {
    let mut warnings = Vec::new();
    let mut harnesses = HarnessCheck::default();
    let path = vars("PATH").unwrap_or_default();
    let available = |program: &str, executable: bool| program_available(program, executable, &path);
    let node = vars("STEVE_NODE")
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            cfg.manifest_node
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "node".into());
    let adapters = resolved_local_adapters(cfg, vars, &node, &path);
    let path_only_bins: std::collections::HashMap<&str, (&str, &str)> =
        steve_harness_spec::builtin_bins()
            .zip(steve_harness_spec::builtin_command_vars())
            .filter(|((id, ..), _)| !steve_harness_spec::is_npm_managed(id))
            .map(|((id, bin, _), (_, var))| (id, (bin, var)))
            .collect();
    for (harness, adapter) in &adapters {
        let js = is_node_script(adapter, &path);
        if !available(adapter, !js) {
            /* an adapter the user left out at setup (`<home>/adapters/selected.json`) is not a problem to report */
            if managed_home_of(std::path::Path::new(adapter))
                .is_some_and(|home| !managed_wanted(&home, harness))
            {
                continue;
            }
            /* a PATH-only built-in CLI the operator never explicitly pointed at a command is optional */
            if let Some((bin, var)) = path_only_bins.get(harness.as_str())
                && adapter == bin
                && vars(var).is_none_or(|v| v.is_empty())
                && !cfg.manifest_adapters.contains(harness)
                && !cfg.declared_adapters.contains_key(harness)
            {
                continue;
            }
            harnesses.missing_adapters.push(harness.clone());
            warnings.push(steve_i18n::t!(
                "hub.check.adapter_missing",
                harness = harness,
                adapter = adapter
            ));
        } else if js && !available(&node, true) {
            harnesses.missing_node = true;
            warnings.push(steve_i18n::t!(
                "hub.check.node_missing",
                node = node,
                harness = harness
            ));
        } else {
            harnesses.resolved_adapters.push(harness.clone());
        }
    }
    harnesses.missing_adapters.sort();
    harnesses.resolved_adapters.sort();
    if crate::wire::principal_state(vars) {
        warnings.push(steve_i18n::t!("hub.check.principal_state"));
    }
    (warnings, harnesses)
}

/// A start makes `d` (`create_dir_all`): it is a directory already, or its nearest existing
/// ancestor is a directory this user can write in.
fn creatable(d: &std::path::Path) -> Result<(), String> {
    if d.exists() {
        return match d.is_dir() {
            true => Ok(()),
            false => Err(format!("{} is not a directory", d.display())),
        };
    }
    let Some(up) = d.ancestors().skip(1).find(|a| a.exists()) else {
        return Ok(());
    };
    if !up.is_dir() {
        return Err(format!(
            "{} cannot be made: {} is not a directory",
            d.display(),
            up.display()
        ));
    }
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(up.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: a NUL-terminated path that lives through the call
    if unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) } != 0 {
        return Err(format!(
            "{} cannot be made: {} is not writable by this user",
            d.display(),
            up.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(kv: &[(&str, &str)]) -> Report {
        let tmp = tempfile::tempdir().expect("tmp");
        let home = tmp.path().to_string_lossy().into_owned();
        let mut kv: Vec<(String, String)> = kv
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for (k, v) in [
            ("STEVE_HOME", home.as_str()),
            ("HOME", home.as_str()),
            ("STEVE_VAULT_KEYSTORE", "file"),
        ] {
            if !kv.iter().any(|(x, _)| x == k) {
                kv.push((k.into(), v.into()));
            }
        }
        for (id, var) in steve_harness_spec::builtin_command_vars() {
            if steve_harness_spec::is_npm_managed(id) && !kv.iter().any(|(x, _)| x == var) {
                kv.push((var.into(), "/usr/bin/false".into()));
            }
        }
        let r = steve_i18n::with(steve_i18n::EN, || {
            check(move |k| kv.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()))
        });
        drop(tmp);
        r
    }

    /// What the check warns about is read by whoever started it: `steve setup` shows it in its own
    /// language, so the warnings come in the language the check runs in, the variable names as they are.
    #[test]
    fn warnings_come_in_the_language_of_their_reader() {
        let vars: crate::wire::Vars = Arc::new(|k: &str| match k {
            "PATH" => Some("/usr/bin:/bin".into()),
            _ => None,
        });
        let cfg = Config::from_vars(|k| match k {
            "STEVE_HOME" => Some("/nonexistent/steve-test/home".into()),
            _ => None,
        })
        .expect("config");
        let zh = steve_i18n::with(steve_i18n::ZH_CN, || local_node_warnings(&cfg, &vars));
        let en = steve_i18n::with(steve_i18n::EN, || local_node_warnings(&cfg, &vars));
        assert_eq!(zh.len(), en.len(), "{zh:?} {en:?}");
        assert!(
            zh.iter().any(|w| w.starts_with("STEVE_PRINCIPAL_STATE=on")
                && w.contains("harness-setup")
                && w.contains("登录")),
            "{zh:?}"
        );
        assert!(
            en.iter().any(|w| w.starts_with("STEVE_PRINCIPAL_STATE=on")
                && w.contains("own sign-in")
                && w.contains("never a copy")),
            "{en:?}"
        );
    }

    #[test]
    fn metrics_rejects_unknown_modes_and_non_loopback_addresses() {
        for kv in [
            vec![("STEVE_METRICS", "other")],
            vec![
                ("STEVE_METRICS", "prometheus"),
                ("STEVE_METRICS_LISTEN", "0.0.0.0:9464"),
            ],
        ] {
            let r = run(&kv);
            assert!(
                !r.ok && r.errors.iter().any(|e| e.starts_with("STEVE_METRICS")),
                "{r:?}"
            );
        }
        let r = run(&[
            ("STEVE_METRICS", "prometheus"),
            ("STEVE_METRICS_LISTEN", "[::1]:9464"),
        ]);
        assert!(r.ok, "{r:?}");
        let r = run(&[
            ("STEVE_METRICS", "prometheus"),
            ("STEVE_METRICS_LISTEN", "unix:/nonexistent/metrics/socket"),
        ]);
        assert!(
            !r.ok
                && r.errors
                    .iter()
                    .any(|e| e.starts_with("STEVE_METRICS_LISTEN")),
            "{r:?}"
        );
    }

    #[test]
    fn a_wildcard_host_is_refused_with_both_ways_out() {
        let r = run(&[("STEVE_HOST", "0.0.0.0")]);
        assert!(!r.ok, "{r:?}");
        let e = &r.errors[0];
        assert!(e.starts_with("STEVE_HOST: "), "{e}");
        assert!(e.contains("STEVE_ALLOW_ANY_HOST=1"), "{e}");
        assert!(e.contains("127.0.0.1"), "{e}");

        let r = run(&[("STEVE_HOST", "0.0.0.0"), ("STEVE_ALLOW_ANY_HOST", "1")]);
        assert!(r.ok, "{r:?}");
        assert_eq!(r.listen, vec!["http://0.0.0.0:8440"]);
    }

    #[test]
    fn listed_addresses_pass_and_a_foreign_one_does_not() {
        let r = run(&[("STEVE_HOST", "127.0.0.1,::1"), ("STEVE_PORT", "9001")]);
        assert!(r.ok, "{r:?}");
        assert_eq!(r.listen, vec!["http://127.0.0.1:9001", "http://[::1]:9001"]);
        // TEST-NET-1 (RFC 5737): never an address of this machine
        let r = run(&[("STEVE_HOST", "127.0.0.1,192.0.2.1")]);
        assert!(!r.ok);
        assert!(r.errors.iter().any(|e| e.contains("192.0.2.1")), "{r:?}");
    }

    #[test]
    fn unconfined_runs_are_a_warning_not_a_refusal() {
        let r = run(&[("STEVE_CONFINE", "sometimes")]);
        assert!(r.ok, "{r:?}");
        assert!(
            r.warnings.iter().any(|w| w.starts_with("STEVE_CONFINE: ")),
            "{r:?}"
        );
        let r = run(&[("STEVE_CONFINE", "off"), ("STEVE_NO_RESUME", "1")]);
        assert!(
            r.warnings.is_empty(),
            "no local Node, no runs of its own: {r:?}"
        );
        if steve_confine::seatbelt_works() {
            let r = run(&[]);
            assert!(
                r.warnings.iter().all(|w| !w.starts_with("STEVE_CONFINE:")),
                "this Mac sandboxes the runs: {r:?}"
            );
        }
    }

    #[test]
    fn principal_login_migration_is_an_actionable_warning() {
        let r = run(&[]);
        assert!(r.ok, "the Hub remains available for setup: {r:?}");
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("own sign-in") && w.contains("harness-setup"))
        );
        let r = run(&[("STEVE_PRINCIPAL_STATE", "off")]);
        assert!(
            r.warnings
                .iter()
                .all(|w| !w.starts_with("STEVE_PRINCIPAL_STATE"))
        );
        let r = run(&[("STEVE_NO_RESUME", "1")]);
        assert!(r.warnings.is_empty(), "no local execution on a copy: {r:?}");
    }

    #[test]
    fn the_other_startup_refusals_are_found_too() {
        let r = run(&[("STEVE_NODE_PROXY_PEERS", "192.0.2.0/24")]);
        assert!(
            r.errors
                .iter()
                .any(|e| e.contains("STEVE_NODE_PROXY_PEERS"))
        );
        let r = run(&[("STEVE_NODE_PROXY_PEERS", "192.0.2.10")]);
        assert!(r.ok, "{r:?}");
        for bad in ["sometimes", "local"] {
            let r = run(&[("STEVE_NODE_MODE", bad)]);
            assert!(
                r.errors.iter().any(|e| e.contains("STEVE_NODE_MODE")),
                "{bad}: {r:?}"
            );
        }
        let r = run(&[("STEVE_PORT", "nope")]);
        assert!(r.errors[0].starts_with("STEVE_PORT"), "{r:?}");
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join("a-file");
        std::fs::write(&file, "").expect("file");
        let home = file.join("home").to_string_lossy().into_owned();
        let r = run(&[
            ("STEVE_HOME", home.as_str()),
            ("STEVE_VAULT_KEYSTORE", "dir"),
        ]);
        assert!(
            r.errors.iter().any(|e| e.contains("is not a directory")),
            "{r:?}"
        );
        let r = run(&[("STEVE_OIDC_CLIENT_SECRET_FILE", "")]);
        assert!(
            r.errors
                .iter()
                .any(|e| e.contains("STEVE_OIDC_CLIENT_SECRET_FILE")),
            "{r:?}"
        );
        let r = run(&[("STEVE_VAULT_KEYSTORE", "dir")]);
        assert!(
            r.errors.iter().any(|e| e.contains("STEVE_VAULT_KEY_DIR")),
            "{r:?}"
        );
    }

    #[test]
    fn review593_re_4_check_config_detects_missing_tailcat() {
        let tmp = tempfile::tempdir().expect("tmp");
        let home = tmp.path().to_string_lossy().into_owned();
        std::fs::create_dir_all(tmp.path().join("tailcat")).expect("tailcat dir");
        std::fs::write(tmp.path().join("tailcat/hub.private.json"), "{}").expect("key");
        let r = run(&[("STEVE_HOME", home.as_str()), ("PATH", "/nonexistent")]);
        assert!(
            r.harnesses.missing_tailcat && r.warnings.iter().any(|w| w.contains("tailcat")),
            "check_config must detect missing tailcat when <home>/tailcat/hub.private.json exists: {r:?}"
        );
        // Once tailcat is present and executable on PATH, missing_tailcat clears.
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        let tc = bin_dir.join("tailcat");
        std::fs::write(&tc, "#!/bin/sh\nexit 0\n").expect("tailcat stub");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tc, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let path = bin_dir.to_string_lossy().into_owned();
        let ok = run(&[("STEVE_HOME", home.as_str()), ("PATH", path.as_str())]);
        assert!(
            !ok.harnesses.missing_tailcat,
            "executable tailcat on PATH must clear missing_tailcat: {ok:?}"
        );
    }

    #[test]
    fn harness_check_reports_missing_adapters_node_and_respects_selection() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tmp");
        let home = tmp.path().to_string_lossy().into_owned();
        let js_adapter = tmp.path().join("adapter.js");
        std::fs::write(&js_adapter, "console.log('ok');\n").expect("js adapter");
        let js_str = js_adapter.to_string_lossy().into_owned();

        // Only claude-code selected; codex left out of selection must not be flagged missing.
        std::fs::create_dir_all(tmp.path().join("adapters")).expect("adapters dir");
        std::fs::write(
            tmp.path().join("adapters/selected.json"),
            r#"["claude-code"]"#,
        )
        .expect("selected.json");

        let r = steve_i18n::with(steve_i18n::EN, || {
            let home_c = home.clone();
            check(move |k| match k {
                "STEVE_HOME" | "HOME" => Some(home_c.clone()),
                "STEVE_VAULT_KEYSTORE" => Some("file".into()),
                "PATH" => Some("/nonexistent".into()),
                _ => None,
            })
        });
        assert!(r.ok, "{r:?}");
        assert_eq!(r.harnesses.missing_adapters, vec!["claude-code"]);
        assert!(!r.harnesses.missing_node);

        // JS adapter present, but node missing from PATH -> missing_node = true.
        let r_missing_node = steve_i18n::with(steve_i18n::EN, || {
            let home_c = home.clone();
            let js_c = js_str.clone();
            check(move |k| match k {
                "STEVE_HOME" | "HOME" => Some(home_c.clone()),
                "STEVE_VAULT_KEYSTORE" => Some("file".into()),
                "STEVE_ADAPTER_CLAUDE" => Some(js_c.clone()),
                "PATH" => Some("/nonexistent".into()),
                _ => None,
            })
        });
        assert!(r_missing_node.harnesses.missing_node, "{r_missing_node:?}");
        assert!(
            r_missing_node.harnesses.missing_adapters.is_empty(),
            "{r_missing_node:?}"
        );

        // Executable node on PATH resolves the JS adapter cleanly.
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        let node = bin_dir.join("node");
        std::fs::write(&node, "#!/bin/sh\nexit 0\n").expect("node stub");
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let path_str = bin_dir.to_string_lossy().into_owned();

        let r_ok = steve_i18n::with(steve_i18n::EN, || {
            let home_c = home.clone();
            let js_c = js_str.clone();
            let path_c = path_str.clone();
            check(move |k| match k {
                "STEVE_HOME" | "HOME" => Some(home_c.clone()),
                "STEVE_VAULT_KEYSTORE" => Some("file".into()),
                "STEVE_ADAPTER_CLAUDE" => Some(js_c.clone()),
                "PATH" => Some(path_c.clone()),
                _ => None,
            })
        });
        assert!(!r_ok.harnesses.missing_node, "{r_ok:?}");
        assert!(r_ok.harnesses.missing_adapters.is_empty(), "{r_ok:?}");
        assert_eq!(r_ok.harnesses.resolved_adapters, vec!["claude-code"]);
    }
}
