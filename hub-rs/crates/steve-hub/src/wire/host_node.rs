//! The Node on the Hub's own machine, as the Hub's routing, YOLO and workspaces see it: its faces
//! ([`Node`]) and the host bridges only a Node sharing this machine's file system answers (its
//! roots, whether a path is still a workspace's directory). Two forms:
//!
//! - [`steve_node::LocalNode`] in the Hub's process (`STEVE_NODE_MODE=local`, the default);
//! - [`ServiceNode`]: the same Node in a process of its own (`STEVE_NODE_MODE=service`, B1), linked
//!   over loopback like a remote Node. Its roots go to it through a file in its home, and the
//!   bridges are answered here, from those roots and the file system both share.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use steve_node::{LocalDir, Root};
use steve_node_api::{Node, NodeId, RootPath, WorkspaceRef, WorkspaceView};

pub trait HostNode: Send + Sync {
    fn id(&self) -> &NodeId;
    fn node(&self) -> Arc<dyn Node>;
    /// The roots the Hub's projects and knowledge folders make now.
    fn set_roots(&self, roots: Vec<Root>);
    /// Whether a prepare of `at` would pass the roots check now.
    fn allows(&self, at: &RootPath) -> bool;
    /// Whether `path`, where the Hub's ledger says `w` was prepared, is still its directory
    /// (`adopted`: an existing directory under a root, not one the Node made).
    fn holds(&self, w: &WorkspaceRef, path: &Path, adopted: bool) -> bool;
    /// Where a workspace this Node just reported prepared is on this machine.
    fn local_dir(&self, view: &WorkspaceView) -> Option<LocalDir>;
    /// Its turns stop when the Hub's process does (a release waits for them).
    fn in_process(&self) -> bool;
}

impl HostNode for steve_node::LocalNode {
    fn id(&self) -> &NodeId {
        Node::id(self)
    }
    fn node(&self) -> Arc<dyn Node> {
        Arc::new(self.clone())
    }
    fn set_roots(&self, roots: Vec<Root>) {
        steve_node::LocalNode::set_roots(self, roots)
    }
    fn allows(&self, at: &RootPath) -> bool {
        steve_node::LocalNode::allows(self, at)
    }
    fn holds(&self, w: &WorkspaceRef, path: &Path, _adopted: bool) -> bool {
        steve_node::LocalNode::holds(self, w, path)
    }
    fn local_dir(&self, view: &WorkspaceView) -> Option<LocalDir> {
        steve_node::LocalNode::local_dir(self, &view.workspace)
    }
    fn in_process(&self) -> bool {
        true
    }
}

/// The Hub's own Node in its own process (B1).
pub struct ServiceNode {
    id: NodeId,
    node: Arc<dyn Node>,
    roots: RwLock<Arc<Vec<Root>>>,
    /// Where the Node reads its roots ([`steve_node::LocalNode::follow_roots_file`]).
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
pub fn write_roots(file: &Path, roots: &[Root]) -> std::io::Result<()> {
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
    fn set_roots(&self, roots: Vec<Root>) {
        if let Err(e) = write_roots(&self.roots_file, &roots) {
            tracing::error!(
                "the local Node's roots ({}): {e}",
                self.roots_file.display()
            );
        }
        *self.roots.write().unwrap() = Arc::new(roots);
    }
    fn allows(&self, at: &RootPath) -> bool {
        steve_node::workspace::resolve(&self.roots.read().unwrap(), at, true).is_ok()
    }
    /// From the shared file system and the roots: the directory is there and (for one under a
    /// root) inside a read-write root now. The Node's own journal is in its process; the ledger's
    /// record of the prepare stands for it.
    fn holds(&self, w: &WorkspaceRef, path: &Path, adopted: bool) -> bool {
        if w.node != self.id {
            return false;
        }
        let roots = self.roots.read().unwrap().clone();
        steve_node::workspace::still_valid(path, &roots, adopted).is_ok()
    }
    /// A worktree or scratch directory the Node made: its display is the path here.
    fn local_dir(&self, view: &WorkspaceView) -> Option<LocalDir> {
        if view.workspace.node != self.id {
            return None;
        }
        let home = std::env::var("HOME").ok();
        let path = std::fs::canonicalize(display_path(&view.display, home.as_deref())?).ok()?;
        path.is_dir().then_some(LocalDir { path, owned: true })
    }
    fn in_process(&self) -> bool {
        false
    }
}
