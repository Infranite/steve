//! What a Node keeps under its home (`$STEVE_HOME`, default `~/.steve-node`):
//!
//! - `node/identity.key`: the Ed25519 seed, 0600 (the Node refuses to start when it is wider)
//! - `node/node.json`: the pairing (hub url, node id, hub id, the pin or `loopback`)
//! - `roots.json`: the directories this Node lets the Hub work in (`[{id, path, display}]`)
//! - `harness.json`: optional, the ACP adapters (`{"adapters": {"claude-code": "/path"}, "node": "node"}`)
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
use std::time::Duration;

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
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Harness {
    #[serde(default)]
    pub adapters: HashMap<String, PathBuf>,
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
            return Ok(Home(PathBuf::from(h)));
        }
        let home = std::env::var("HOME").context("HOME is not set")?;
        Ok(Home(Path::new(&home).join(".steve-node")))
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
        let p = self.0.join("harness.json");
        if !p.exists() {
            return Ok(Harness::default());
        }
        Ok(serde_json::from_str(&std::fs::read_to_string(&p)?)?)
    }

    pub fn acp(&self) -> Result<steve_acp::AcpConfig> {
        let h = self.harness()?;
        let user_home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let mut adapters = h.adapters;
        adapters
            .entry("claude-code".into())
            .or_insert_with(|| "claude-code-acp".into());
        Ok(steve_acp::AcpConfig {
            adapters,
            node: h.node.unwrap_or_else(|| "node".into()),
            claude_executable: h.claude_executable.unwrap_or_else(|| "claude".into()),
            steve_home: self.0.clone(),
            codex_home: user_home.join(".codex"),
            user_home,
            version: option_env!("STEVE_BUILD_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .into(),
            idle_release: Duration::from_secs(600),
            retry_delays: vec![Duration::from_secs(20), Duration::from_secs(60)],
        })
    }

    pub fn node_config(&self, node: NodeId) -> Result<steve_node::NodeConfig> {
        Ok(steve_node::NodeConfig {
            node,
            home: self.local(),
            acp: self.acp()?,
            roots: self.roots()?,
            // N2: a remote Node hosts sessions only; jobs and execs stay local to the Hub
            capacity: steve_node_api::Capacity {
                sessions: 16,
                jobs: 0,
                execs: 0,
            },
            storage: steve_node::Storage::default(),
            jobs: Default::default(),
            ceiling: self.ceiling()?,
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
}
