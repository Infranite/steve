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
            };
        }
    };
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        if d.exists() && !d.is_dir() {
            errors.push(format!("STEVE_HOME: {} is not a directory", d.display()));
        }
    }
    if let Err(e) = crate::wire::local_node_mode(&vars, cfg.no_resume) {
        errors.push(e.to_string());
    }
    if let Err(e) = steve_modules::vault::keystore::from_env(&cfg.home, |k| vars(k)) {
        errors.push(format!("vault key store (STEVE_VAULT_KEYSTORE): {e}"));
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
    }
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
            ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false"),
            ("STEVE_ADAPTER_CODEX", "/usr/bin/false"),
            ("STEVE_VAULT_KEYSTORE", "file"),
        ] {
            if !kv.iter().any(|(x, _)| x == k) {
                kv.push((k.into(), v.into()));
            }
        }
        let r = check(move |k| kv.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()));
        drop(tmp);
        r
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
    fn the_other_startup_refusals_are_found_too() {
        let r = run(&[("STEVE_NODE_MODE", "sometimes")]);
        assert!(
            r.errors.iter().any(|e| e.contains("STEVE_NODE_MODE")),
            "{r:?}"
        );
        let r = run(&[("STEVE_PORT", "nope")]);
        assert!(r.errors[0].starts_with("STEVE_PORT"), "{r:?}");
        let r = run(&[("STEVE_VAULT_KEYSTORE", "dir")]);
        assert!(
            r.errors.iter().any(|e| e.contains("STEVE_VAULT_KEY_DIR")),
            "{r:?}"
        );
    }
}
