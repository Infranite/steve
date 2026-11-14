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
    let mut warnings = Vec::new();
    match crate::wire::local_node_mode(&vars, cfg.no_resume) {
        Err(e) => errors.push(e.to_string()),
        // the local Node's runs in the OS sandbox (never a reason not to start)
        Ok(true) => {
            warnings.extend(local_node_warnings(&cfg, &vars));
            if let Err(why) = steve_node::confinement(
                &cfg.home,
                &cfg.data_dir.join("node"),
                vars("STEVE_CONFINE"),
            ) {
                warnings.push(steve_i18n::t!("hub.check.unconfined", why = why));
            }
        }
        Ok(false) => {}
    }
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
    }
}

/// Setup guidance shared by preflight and a normal boot. No harness is spawned and no login
/// contents are inspected: the operator, not Steve, authenticates each principal's harness.
pub(crate) fn local_node_warnings(cfg: &Config, vars: &crate::wire::Vars) -> Vec<String> {
    let mut warnings = Vec::new();
    let path = vars("PATH").unwrap_or_default();
    let available = |program: &str, executable: bool| {
        use std::os::unix::fs::PermissionsExt;
        let usable = |p: &std::path::Path| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && (!executable || m.permissions().mode() & 0o111 != 0))
        };
        let p = std::path::Path::new(program);
        if p.components().count() > 1 {
            usable(p)
        } else {
            std::env::split_paths(&path).any(|d| usable(&d.join(p)))
        }
    };
    let path_only_bins: std::collections::HashMap<&str, (&str, &str)> =
        steve_acp::adapters::builtin_bins()
            .zip(steve_acp::adapters::builtin_commands(std::path::Path::new(
                "",
            )))
            .filter(|((id, ..), _)| !steve_acp::adapters::is_npm_managed(id))
            .map(|((id, bin, _), (_, var, _))| (id, (bin, var)))
            .collect();
    for (harness, adapter) in &cfg.adapters {
        let js = steve_acp::is_node_script(std::path::Path::new(adapter));
        if !available(adapter, !js) {
            /* an adapter the user left out at setup (steve_acp::managed) is not a problem to report */
            if steve_acp::managed::home_of(std::path::Path::new(adapter))
                .is_some_and(|home| !steve_acp::managed::wanted(&home, harness))
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
            warnings.push(steve_i18n::t!(
                "hub.check.adapter_missing",
                harness = harness,
                adapter = adapter
            ));
        } else if js && !available(&cfg.node, true) {
            warnings.push(steve_i18n::t!(
                "hub.check.node_missing",
                node = cfg.node,
                harness = harness
            ));
        }
    }
    if crate::wire::principal_state(vars) {
        warnings.push(steve_i18n::t!("hub.check.principal_state"));
    }
    warnings
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
        for (id, var, _) in steve_acp::adapters::builtin_commands(std::path::Path::new("")) {
            if steve_acp::adapters::is_npm_managed(id) && !kv.iter().any(|(x, _)| x == var) {
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
            "STEVE_ADAPTER_CODEX" => Some("/nonexistent/steve-test/codex-acp".into()),
            v if v.starts_with("STEVE_ADAPTER_") => Some("/usr/bin/false".into()),
            _ => None,
        })
        .expect("config");
        let zh = steve_i18n::with(steve_i18n::ZH_CN, || local_node_warnings(&cfg, &vars));
        let en = steve_i18n::with(steve_i18n::EN, || local_node_warnings(&cfg, &vars));
        assert_eq!(zh.len(), en.len(), "{zh:?} {en:?}");
        assert!(
            zh.iter().any(|w| w.contains("ACP 适配器 codex")
                && w.contains("/nonexistent/steve-test/codex-acp")),
            "{zh:?}"
        );
        assert!(
            zh.iter().any(|w| w.starts_with("STEVE_PRINCIPAL_STATE=on")
                && w.contains("harness-setup")
                && w.contains("登录")),
            "{zh:?}"
        );
        assert!(
            en.iter().any(|w| w.starts_with("STEVE_PRINCIPAL_STATE=on")
                && w.contains("Harness CLI logins")
                && w.contains("not copied")),
            "{en:?}"
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
        let r = run(&[("STEVE_CONFINE", "off"), ("STEVE_NODE_MODE", "off")]);
        assert!(
            r.warnings.is_empty(),
            "no local Node, no runs of its own: {r:?}"
        );
        if steve_vault::harness::seatbelt_works() {
            let r = run(&[]);
            assert!(
                r.warnings.iter().all(|w| !w.starts_with("STEVE_CONFINE:")),
                "this Mac sandboxes the runs: {r:?}"
            );
        }
    }

    #[test]
    fn missing_adapters_and_principal_login_migration_are_actionable_warnings() {
        let r = run(&[("STEVE_ADAPTER_CODEX", "/nonexistent/steve-test/codex-acp")]);
        assert!(r.ok, "the Hub remains available for setup: {r:?}");
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("ACP adapter codex") && w.contains("install its ACP adapter"))
        );
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("Harness CLI logins") && w.contains("harness-setup"))
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
    fn an_adapter_left_out_at_setup_is_not_reported_missing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let home = tmp.path().to_string_lossy().into_owned();
        // where Steve itself would have installed it; nothing is there
        let own = steve_acp::managed::command(tmp.path(), "codex").expect("built in");
        let own = own.to_string_lossy().into_owned();
        let missing = |r: &Report| r.warnings.iter().any(|w| w.contains("ACP adapter codex"));
        let own_install = || {
            run(&[
                ("STEVE_HOME", home.as_str()),
                ("STEVE_ADAPTER_CODEX", own.as_str()),
            ])
        };
        assert!(
            missing(&own_install()),
            "nothing was ever chosen: all of them are expected"
        );
        let chosen = steve_acp::managed::dir(tmp.path()).join("selected.json");
        std::fs::create_dir_all(chosen.parent().expect("dir")).expect("dir");
        std::fs::write(&chosen, r#"["codex"]"#).expect("choice");
        assert!(missing(&own_install()), "chosen and not there");
        std::fs::write(&chosen, r#"["claude-code"]"#).expect("choice");
        let r = own_install();
        assert!(r.ok && !missing(&r), "{r:?}");
        // the same choice does not hide an adapter configured somewhere else
        let r = run(&[
            ("STEVE_HOME", home.as_str()),
            ("STEVE_ADAPTER_CODEX", "/nonexistent/steve-test/codex-acp"),
        ]);
        assert!(missing(&r), "{r:?}");
        // a PATH-only adapter left unconfigured stays quiet, unless declared in harness.json
        let missing_cursor =
            |r: &Report| r.warnings.iter().any(|w| w.contains("ACP adapter cursor"));
        assert!(!missing_cursor(&r), "{r:?}");
        std::fs::write(
            tmp.path().join("harness.json"),
            r#"{"adapters": {"cursor": "agent"}}"#,
        )
        .expect("manifest");
        let r = run(&[("STEVE_HOME", home.as_str()), ("PATH", "/nonexistent")]);
        assert!(missing_cursor(&r), "{r:?}");
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
        let r = run(&[("STEVE_NODE_MODE", "sometimes")]);
        assert!(
            r.errors.iter().any(|e| e.contains("STEVE_NODE_MODE")),
            "{r:?}"
        );
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
}
