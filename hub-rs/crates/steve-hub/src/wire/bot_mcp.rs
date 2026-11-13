//! The MCP servers of the library a Bot uses, on their way to its sessions' harnesses
//! (`steve_modules::caps::mcp`).
//!
//! When a session is opened or reopened on a Node ([`super::node_runtime`]), the servers its Bot is
//! assigned and has switched on go into [`OpenSession::mcp_servers`] as they are then, every secret
//! as a reference (`<server>#<label>`). The Node takes each value under `mcp:<session>` as its
//! harness opens ([`super::node_secrets::SessionSecrets`]): only a Node the session is bound to,
//! only a secret of a server the session's Bot has switched on, decrypted then and kept nowhere. The same path serves the
//! Hub's own Node and remote ones, and every harness: the servers are ACP's, not any harness's.

use rusqlite::{Connection, OptionalExtension};
use steve_modules::caps::Capabilities;
use steve_modules::caps::mcp::secret_id;
use steve_node_api::{McpServer, McpTransport, McpVar};
use steve_store::Store;
use steve_store::caps::mcp::{self as repo, Server, Var};
use zeroize::Zeroizing;

use super::node_secrets::SessionSecrets;

/// The ACP-level server a harness is given, its secrets as references.
pub fn node_server(s: &Server) -> McpServer {
    let vars = |vs: &[Var]| -> Vec<McpVar> {
        vs.iter()
            .map(|v| McpVar {
                name: v.name.clone(),
                value: if v.secret {
                    String::new()
                } else {
                    v.value.clone().unwrap_or_default()
                },
                secret: v
                    .secret
                    .then(|| secret_id(&s.id, &repo::secret_label(&s.transport, &v.name))),
            })
            .collect()
    };
    let transport = match s.transport.as_str() {
        "stdio" => McpTransport::Stdio {
            command: s.command.clone().unwrap_or_default(),
            args: s.args.clone(),
            env: vars(&s.env),
        },
        "sse" => McpTransport::Sse {
            url: s.url.clone().unwrap_or_default(),
            headers: vars(&s.headers),
        },
        _ => McpTransport::Http {
            url: s.url.clone().unwrap_or_default(),
            headers: vars(&s.headers),
        },
    };
    McpServer {
        name: s.name.clone(),
        transport,
    }
}

/// The servers `session`'s harness is given: the ones its Bot has switched on, as they are now.
pub fn for_session(c: &Connection, session: &str) -> steve_store::Result<Vec<McpServer>> {
    Ok(repo::enabled_for_session(c, session)?
        .iter()
        .map(node_server)
        .collect())
}

/// The sessions' MCP secrets for the Nodes ([`super::node_secrets::NodeSecrets::set_sessions`]).
pub struct McpSecrets {
    store: Store,
    svc: Capabilities,
}

impl McpSecrets {
    pub fn new(store: Store, svc: Capabilities) -> std::sync::Arc<McpSecrets> {
        std::sync::Arc::new(McpSecrets { store, svc })
    }
}

impl SessionSecrets for McpSecrets {
    fn value(&self, node: &str, session: &str, id: &str) -> Option<Zeroizing<Vec<u8>>> {
        let bound: Option<i64> = self
            .store
            .reader(|c| {
                c.query_row(
                    "SELECT 1 FROM runtime_sessions WHERE session_id = ? AND node_id = ?",
                    [session, node],
                    |r| r.get(0),
                )
                .optional()
                .map_err(steve_store::StoreError::from)
            })
            .ok()
            .flatten();
        bound?;
        match self.svc.mcp_secret(session, id) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(session, node, "MCP secret not given: {e}");
                None
            }
        }
    }
}
