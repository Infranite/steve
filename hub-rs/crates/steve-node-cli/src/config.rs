//! What a Node keeps under its home (`$STEVE_HOME`, default `~/.steve-node`):
//!
//! - `node/identity.key`: the Ed25519 seed, 0600 (the Node refuses to start when it is wider)
//! - `node/node.json`: the pairing (hub url, node id, hub id, the pin or `loopback`)
//! - `roots.json`: the directories this Node lets the Hub work in (`[{id, path, display}]`)
//! - `harness.json`: optional, the ACP adapters (`{"adapters": {"claude-code": "/path"}, "node": "node"}`);
//!   an adapter of one's own is declared as `{"adapters": {"my-agent": {"command": "/path",
//!   "name": "My Agent", "args": [], "env": {}, "properties": {}}}}` ([`steve_acp::adapters`])
//! - `policy.json`: optional, the most any Hub may ask of this Node
//!   (`{"max_permissions": "auto", "full_network": true, "exec": false, "jobs": false}`, the
//!   defaults). Sessions here run with this machine's own home and logins until runs are
//!   sandboxed: only `"max_permissions": "full_access"` lets the Hub turn permission prompts off
//!   (`bypassPermissions`). Read at start; a file that does not parse stops the Node.
//! - `node/local/`: the LocalNode's journal, worktrees and caches
//!
//! No secret ever goes through argv: the pairing code comes from stdin or `STEVE_PAIR_CODE`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use steve_node::Root;
use steve_node_api::{NodeId, RootId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pairing {
    /// `https://hub.example` (or `http://127.0.0.1:port` with the loopback flag, or
    /// `tailcat:<address>:<port>`).
    pub hub_url: String,
    pub node_id: String,
    pub hub_id: String,
    /// `sha256:<hex>` of the Hub's certificate, when pairing gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// Paired over plain loopback (same machine, `STEVE_NODE_LOOPBACK_PLAINTEXT=1`).
    #[serde(default)]
    pub loopback: bool,
    /// The Hub's identity key (hex, M1): from pairing, or remembered from the first welcome it
    /// signed. Known, a welcome without its signature is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub_key: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Harness {
    #[serde(default)]
    pub adapters: HashMap<String, steve_acp::AdapterEntry>,
    #[serde(default)]
    pub node: Option<PathBuf>,
    #[serde(default)]
    pub claude_executable: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootEntry {
    pub id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub display: Option<String>,
    /// `read_only`: listed, read and scanned, never a workspace.
    #[serde(default)]
    pub access: steve_node_api::RootAccess,
}

pub struct Home(pub PathBuf);

impl Home {
    /// `$STEVE_HOME`, else `~/.steve-node`.
    pub fn from_env() -> Result<Home> {
        if let Ok(h) = std::env::var("STEVE_HOME")
            && !h.is_empty()
        {
            let p = PathBuf::from(h);
            return Ok(Home(std::path::absolute(&p).unwrap_or(p)));
        }
        let home = std::env::var("HOME").context("HOME is not set")?;
        let p = Path::new(&home).join(".steve-node");
        Ok(Home(std::path::absolute(&p).unwrap_or(p)))
    }
    pub fn identity(&self) -> PathBuf {
        self.0.join("node/identity.key")
    }
    pub fn pairing_file(&self) -> PathBuf {
        self.0.join("node/node.json")
    }
    pub fn local(&self) -> PathBuf {
        self.0.join("node/local")
    }

    pub fn pairing(&self) -> Result<Pairing> {
        let p = self.pairing_file();
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("not paired yet ({} is missing)", p.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save_pairing(&self, p: &Pairing) -> Result<()> {
        let f = self.pairing_file();
        std::fs::create_dir_all(f.parent().unwrap())?;
        std::fs::write(&f, serde_json::to_string_pretty(p)?)?;
        Ok(())
    }

    pub fn roots(&self) -> Result<Vec<Root>> {
        let p = self.0.join("roots.json");
        if !p.exists() {
            return Ok(vec![]);
        }
        let entries: Vec<RootEntry> = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
        entries
            .into_iter()
            .map(|e| {
                if !e.path.is_absolute() {
                    bail!("root {} must be an absolute path", e.id);
                }
                Ok(Root {
                    id: RootId::new(e.id.clone())
                        .map_err(|_| anyhow::anyhow!("bad root id {}", e.id))?,
                    display: e.display.unwrap_or_else(|| e.path.display().to_string()),
                    path: e.path,
                    access: e.access,
                })
            })
            .collect()
    }

    /// The owner's ceiling (`policy.json`; none: [`steve_node::PolicyCeiling::default`]).
    pub fn ceiling(&self) -> Result<steve_node::PolicyCeiling> {
        let p = self.0.join("policy.json");
        if !p.exists() {
            return Ok(steve_node::PolicyCeiling::default());
        }
        serde_json::from_str(&std::fs::read_to_string(&p)?)
            .with_context(|| format!("{} does not parse", p.display()))
    }

    pub fn harness(&self) -> Result<Harness> {
        self.harness_from_vars(|k| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
    }

    pub fn harness_from_vars(&self, var: impl Fn(&str) -> Option<PathBuf>) -> Result<Harness> {
        let p = var("STEVE_HARNESS_MANIFEST").unwrap_or_else(|| self.0.join("harness.json"));
        if !p.exists() {
            return Ok(Harness::default());
        }
        Ok(serde_json::from_str(&std::fs::read_to_string(&p)?)?)
    }

    pub fn acp(&self) -> Result<steve_acp::AcpConfig> {
        let user_home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let path_env = std::env::var_os("PATH");
        self.acp_from_vars(
            |k| {
                std::env::var_os(k)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            },
            path_env.as_deref(),
            user_home,
            || Some(steve_acp::npm_root()),
        )
    }

    pub fn acp_from_vars(
        &self,
        var: impl Fn(&str) -> Option<PathBuf>,
        path_env: Option<&std::ffi::OsStr>,
        user_home: PathBuf,
        mut npm_root: impl FnMut() -> Option<PathBuf>,
    ) -> Result<steve_acp::AcpConfig> {
        let h = self.harness_from_vars(&var)?;
        let (declared_commands, declared) = steve_acp::adapters::split_entries(h.adapters);
        let steve_home = std::path::absolute(&self.0).unwrap_or_else(|_| self.0.clone());
        let mut root: Option<Option<PathBuf>> = None;
        let adapters = steve_acp::resolve_builtin_adapters(
            &steve_home,
            &declared_commands,
            &var,
            path_env,
            || {
                root.get_or_insert_with(&mut npm_root)
                    .clone()
                    .unwrap_or_default()
            },
        );
        let node = var("STEVE_NODE")
            .or(h.node)
            .or_else(|| steve_acp::which_in("node", path_env))
            .unwrap_or_else(|| "node".into());
        let claude_executable = var("CLAUDE_CODE_EXECUTABLE")
            .or(h.claude_executable)
            .or_else(|| steve_acp::default_claude_executable(path_env))
            .unwrap_or_default();
        let codex_home = var("CODEX_HOME").unwrap_or_else(|| user_home.join(".codex"));
        Ok(steve_acp::AcpConfig {
            declared,
            adapters,
            node,
            claude_executable,
            steve_home,
            codex_home,
            user_home,
            version: option_env!("STEVE_BUILD_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .into(),
        })
    }

    pub fn node_config(&self, node: NodeId) -> Result<steve_node::NodeConfig> {
        let ceiling = self.ceiling()?;
        Ok(steve_node::NodeConfig {
            node,
            home: self.local(),
            acp: self.acp()?,
            roots: self.roots()?,
            // sessions, and commands (flow steps, vault uses, pushes) when its owner lets them run
            // here (`policy.json` `exec`, B4); jobs stay with the Hub
            capacity: steve_node_api::Capacity {
                sessions: steve_node_api::Capacity::SESSIONS_DEFAULT,
                jobs: 0,
                execs: if ceiling.exec { 4 } else { 0 },
            },
            storage: steve_node::Storage::default(),
            ceiling,
        })
    }
}

/// `https://h` → `wss://h/node/ws`, `http://h` → `ws://h/node/ws`.
pub fn ws_url(hub_url: &str) -> Result<String> {
    let base = hub_url.trim_end_matches('/');
    if let Some(r) = base.strip_prefix("https://") {
        Ok(format!("wss://{r}/node/ws"))
    } else if let Some(r) = base.strip_prefix("http://") {
        Ok(format!("ws://{r}/node/ws"))
    } else {
        bail!("the hub url must start with https:// (or http:// to loopback)")
    }
}

pub fn loopback_plaintext_allowed() -> bool {
    std::env::var("STEVE_NODE_LOOPBACK_PLAINTEXT").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_node_api::PermissionMode;

    #[test]
    fn standalone_node_defaults_to_32_turns_without_widening_other_resources() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home(dir.path().to_path_buf());
        let cfg = home.node_config(NodeId::new("n1").unwrap()).unwrap();
        assert_eq!(cfg.capacity.sessions, 32);
        assert_eq!(cfg.capacity.jobs, 0);
        assert_eq!(cfg.capacity.execs, 0);
        assert_eq!(cfg.ceiling, steve_node::PolicyCeiling::default());
    }

    /// H1: without a policy file the Node takes no FullAccess, commands or jobs; the owner widens
    /// it, and a file that does not parse (a typo included) stops the Node rather than default.
    #[test]
    fn the_ceiling_is_narrow_unless_the_owner_widens_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home(dir.path().to_path_buf());
        let c = home.ceiling().unwrap();
        assert_eq!(c, steve_node::PolicyCeiling::default());
        assert_eq!(c.max_permissions, PermissionMode::Auto);
        assert!(c.full_network && !c.exec && !c.jobs);
        std::fs::write(
            dir.path().join("policy.json"),
            r#"{"max_permissions": "full_access"}"#,
        )
        .unwrap();
        let c = home.ceiling().unwrap();
        assert_eq!(c.max_permissions, PermissionMode::FullAccess);
        assert!(c.full_network && !c.exec && !c.jobs);
        std::fs::write(
            dir.path().join("policy.json"),
            r#"{"max_permissions": "ask", "full_network": false}"#,
        )
        .unwrap();
        let c = home.ceiling().unwrap();
        assert_eq!(c.max_permissions, PermissionMode::Ask);
        assert!(!c.full_network);
        for bad in [
            r#"{"max_permission": "full_access"}"#,
            r#"{"max_permissions": "bypassPermissions"}"#,
            "not json",
        ] {
            std::fs::write(dir.path().join("policy.json"), bad).unwrap();
            assert!(home.ceiling().is_err(), "{bad}");
        }
        assert!(home.node_config(NodeId::new("n1").unwrap()).is_err());
    }

    /// A `harness.json` written for an older Node still reads; an adapter of one's own is declared
    /// next to the built-in ones and keeps its arguments, variables and properties.
    #[test]
    fn harness_json_takes_paths_and_declared_adapters() {
        let dir = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let fake_acp = bin.path().join("claude-agent-acp");
        std::fs::write(&fake_acp, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_acp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_env = bin.path().as_os_str();
        let home = Home(dir.path().to_path_buf());
        std::fs::write(
            dir.path().join("harness.json"),
            r#"{"adapters": {"codex": "/opt/codex-acp"}, "node": "/opt/node"}"#,
        )
        .unwrap();
        let acp = home
            .acp_from_vars(|_| None, Some(path_env), dir.path().to_path_buf(), || None)
            .unwrap();
        assert_eq!(acp.adapters["codex"], PathBuf::from("/opt/codex-acp"));
        // the default adapter is still looked up on PATH
        assert_eq!(
            acp.adapters
                .get(steve_acp::adapters::DEFAULT_ADAPTER.0)
                .map(PathBuf::as_path),
            Some(fake_acp.as_path())
        );
        assert!(acp.declared.is_empty());
        std::fs::write(
            dir.path().join("harness.json"),
            r#"{"adapters": {"mine": {"command": "/opt/mine", "name": "Mine", "args": ["acp"],
                "env": {"MINE_MODE": "acp"}, "properties": {"modes": {"ask": "ask"}}}}}"#,
        )
        .unwrap();
        let acp = home
            .acp_from_vars(|_| None, Some(path_env), dir.path().to_path_buf(), || None)
            .unwrap();
        assert_eq!(acp.adapters["mine"], PathBuf::from("/opt/mine"));
        let spec = acp.adapter_spec("mine");
        assert_eq!(spec.name, "Mine");
        assert!(!spec.builtin);
        assert_eq!(acp.adapter_mode("mine", PermissionMode::Ask), "ask");
        let (_, args) = acp.adapter_command("mine").unwrap();
        assert_eq!(args, vec![std::ffi::OsString::from("acp")]);
        assert_eq!(
            acp.adapter_env("mine"),
            vec![("MINE_MODE".into(), "acp".into())]
        );
    }
}
