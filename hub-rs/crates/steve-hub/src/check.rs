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

fn is_executable_file(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

fn program_available(program: &str, path: &str) -> bool {
    let p = std::path::Path::new(program);
    if p.components().count() > 1 {
        is_executable_file(p)
    } else {
        std::env::split_paths(path)
            .map(|d| d.join(p))
            .any(|candidate| is_executable_file(&candidate))
    }
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
    if !program_available(program, &path) {
        harnesses.missing_tailcat = true;
        warnings.push(steve_i18n::t!(
            "hub.check.tailcat_missing",
            tailcat = program
        ));
    }
}

/// Timeout for invoking the sibling `steve-node harness-probe` binary during offline preflight.
const HARNESS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Per-stream output cap for `steve-node harness-probe` (a compact JSON preflight summary; 1 MiB
/// bounds unexpected output without truncating valid reports).
const HARNESS_PROBE_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

fn sibling_steve_node() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.with_file_name("steve-node");
    if is_executable_file(&candidate) {
        return Some(candidate);
    }
    // Under `cargo test`, unit-test binaries live in `target/<profile>/deps/`; check parent dir too.
    if let Some(parent) = exe.parent().and_then(std::path::Path::parent) {
        let sibling = parent.join("steve-node");
        if is_executable_file(&sibling) {
            return Some(sibling);
        }
    }
    None
}

/// Delegates offline harness & Node.js resolution to `steve-node harness-probe` (which uses
/// `steve-acp` as the single source of truth) so `steve-hub` never duplicates adapter discovery.
fn probe_harnesses_via_node(
    cfg: &Config,
    vars: &crate::wire::Vars,
) -> Option<steve_node_api::HarnessPreflight> {
    if let Some(bin) = sibling_steve_node() {
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg("harness-probe")
            .env_clear()
            .env("STEVE_HOME", &cfg.home)
            .env("STEVE_HARNESS_MANIFEST", &cfg.harness_manifest);
        for key in ["HOME", "PATH", "STEVE_NODE"] {
            if let Some(v) = vars(key) {
                cmd.env(key, v);
            }
        }
        for (_, env_var) in steve_harness_spec::builtin_command_vars() {
            if let Some(v) = vars(env_var) {
                cmd.env(env_var, v);
            }
        }
        if let Ok(out) = steve_runtime::process::run_bounded(
            &mut cmd,
            "steve-node harness-probe",
            steve_runtime::process::BoundedOptions {
                max_output_bytes: HARNESS_PROBE_MAX_OUTPUT_BYTES,
                ..steve_runtime::process::BoundedOptions::capture(
                    "harness_probe",
                    HARNESS_PROBE_TIMEOUT,
                )
            },
        ) && out.status.success()
            && let Ok(report) =
                serde_json::from_slice::<steve_node_api::HarnessPreflight>(&out.stdout)
        {
            return Some(report);
        }
    }
    #[cfg(test)]
    {
        return Some(steve_node_cli::local_node::probe_harnesses(|k| {
            if k == "STEVE_HOME" {
                Some(cfg.home.to_string_lossy().into_owned())
            } else if k == "STEVE_HARNESS_MANIFEST" {
                Some(cfg.harness_manifest.to_string_lossy().into_owned())
            } else {
                vars(k)
            }
        }));
    }
    #[cfg(not(test))]
    {
        None
    }
}

fn local_node_checks(cfg: &Config, vars: &crate::wire::Vars) -> (Vec<String>, HarnessCheck) {
    let mut warnings = Vec::new();
    let mut harnesses = HarnessCheck::default();
    if let Some(preflight) = probe_harnesses_via_node(cfg, vars) {
        for missing in preflight.missing_adapters {
            harnesses.missing_adapters.push(missing.harness.clone());
            warnings.push(steve_i18n::t!(
                "hub.check.adapter_missing",
                harness = missing.harness,
                adapter = missing.adapter
            ));
        }
        for missing in preflight.missing_node {
            harnesses.missing_node = true;
            warnings.push(steve_i18n::t!(
                "hub.check.node_missing",
                node = missing.node,
                harness = missing.harness
            ));
        }
        harnesses.resolved_adapters = preflight.resolved_adapters;
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
