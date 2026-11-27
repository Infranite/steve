//! The Node on the Hub's own machine, as the Hub's routing and workspaces see it: its faces
//! ([`Node`]) and the host metadata only a Node sharing this machine's configuration needs (its
//! roots file and pure-in-memory root membership checks). The colocated Node runs in its own
//! `steve-node --local-node` process and links to the Hub over loopback like a paired Node.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use steve_node_api::{
    HostRoot, LocalDir, Node, NodeId, RootAccess, RootPath, WorkspaceRef, WorkspaceView,
};

pub trait HostNode: Send + Sync {
    fn id(&self) -> &NodeId;
    fn node(&self) -> Arc<dyn Node>;
    /// The roots the Hub's projects and knowledge folders make now.
    fn set_roots(&self, roots: Vec<HostRoot>);
    /// Whether a prepare of `at` would pass the roots check now (pure allowlist check; the Node
    /// validates the filesystem path itself when preparing).
    fn allows(&self, at: &RootPath) -> bool;
    /// Whether `path`, where the Hub's ledger says `w` was prepared, is still allowed by the
    /// configured roots (`adopted`: an existing directory under a root, not one the Node made).
    fn holds(&self, w: &WorkspaceRef, path: &Path, adopted: bool) -> bool;
    /// Where a workspace this Node just reported prepared is on this machine (pure path decode).
    fn local_dir(&self, view: &WorkspaceView) -> Option<LocalDir>;
}

/// The Hub's own Node in its own process (`steve-node --local-node`).
pub struct ServiceNode {
    id: NodeId,
    node: Arc<dyn Node>,
    roots: RwLock<Arc<Vec<HostRoot>>>,
    /// Where the Node reads its roots (`steve-node` follows this file).
    roots_file: PathBuf,
}

/// The absolute path a local Node's display stands for (`~` is `home`, as the Node writes it).
pub(crate) fn display_path(display: &str, home: Option<&str>) -> Option<PathBuf> {
    let path = match display.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(format!("{}{rest}", home.filter(|h| !h.is_empty())?))
        }
        Some(_) => return None,
        None => PathBuf::from(display),
    };
    path.is_absolute().then_some(path)
}

/// The roots file in the service Node's home.
pub fn roots_file(node_home: &Path) -> PathBuf {
    node_home.join("host-roots.json")
}

impl ServiceNode {
    pub fn new(id: NodeId, node: Arc<dyn Node>, node_home: &Path) -> Arc<ServiceNode> {
        Arc::new(ServiceNode {
            id,
            node,
            roots: RwLock::new(Arc::new(vec![])),
            roots_file: roots_file(node_home),
        })
    }
}

/// Writes `roots` where the service Node reads them: a temporary file renamed over the old one,
/// so it never reads half of it.
pub fn write_roots(file: &Path, roots: &[HostRoot]) -> std::io::Result<()> {
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(roots)?)?;
    std::fs::rename(&tmp, file)
}

impl HostNode for ServiceNode {
    fn id(&self) -> &NodeId {
        &self.id
    }
    fn node(&self) -> Arc<dyn Node> {
        self.node.clone()
    }
    fn set_roots(&self, roots: Vec<HostRoot>) {
        if let Err(e) = write_roots(&self.roots_file, &roots) {
            tracing::error!(
                "the local Node's roots ({}): {e}",
                self.roots_file.display()
            );
        }
        *self.roots.write().unwrap() = Arc::new(roots);
    }
    fn allows(&self, at: &RootPath) -> bool {
        let roots = self.roots.read().unwrap();
        roots
            .iter()
            .any(|r| r.id == at.root && r.access == RootAccess::ReadWrite)
    }
    /// Pure allowlist check against configured roots; the Node itself validates filesystem
    /// existence and symlinks when opening the workspace.
    fn holds(&self, w: &WorkspaceRef, path: &Path, adopted: bool) -> bool {
        if w.node != self.id {
            return false;
        }
        if !adopted {
            return true;
        }
        let roots = self.roots.read().unwrap();
        roots
            .iter()
            .any(|r| r.access == RootAccess::ReadWrite && path.starts_with(&r.path))
    }
    /// A worktree or scratch directory the Node made: its display is the path here.
    fn local_dir(&self, view: &WorkspaceView) -> Option<LocalDir> {
        if view.workspace.node != self.id {
            return None;
        }
        let home = std::env::var("HOME").ok();
        let path = display_path(&view.display, home.as_deref())?;
        Some(LocalDir { path, owned: true })
    }
}

#[cfg(test)]
impl HostNode for steve_node::LocalNode {
    fn id(&self) -> &NodeId {
        steve_node_api::Node::id(self)
    }
    fn node(&self) -> Arc<dyn Node> {
        Arc::new(self.clone())
    }
    fn set_roots(&self, roots: Vec<HostRoot>) {
        let roots = roots
            .into_iter()
            .map(|r| steve_node::Root {
                id: r.id,
                display: r.display,
                path: r.path,
                access: r.access,
            })
            .collect();
        steve_node::LocalNode::set_roots(self, roots);
    }
    fn allows(&self, at: &RootPath) -> bool {
        steve_node::LocalNode::allows(self, at)
    }
    fn holds(&self, w: &WorkspaceRef, path: &Path, _adopted: bool) -> bool {
        steve_node::LocalNode::holds(self, w, path)
    }
    fn local_dir(&self, view: &WorkspaceView) -> Option<LocalDir> {
        steve_node::LocalNode::local_dir(self, &view.workspace).map(|d| LocalDir {
            path: d.path,
            owned: d.owned,
        })
    }
}
