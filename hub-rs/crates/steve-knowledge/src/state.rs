//! What the knowledge module needs from the rest of the Hub, beyond [`steve_modules::Modules`]:
//! a way to emit the
//! `{type:"kb"}` SSE event (not yet a `ChangeEvent` variant), and the document loaders the index
//! queue reads bodies with (notes and folders always; files need the data directory, see
//! [`crate::sources::files::FilesLoader`]).

use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use tokio::sync::mpsc;

use crate::index::DocLoader;
use crate::repo::DocRow;
use crate::sources::folder::FolderPolicy;
use crate::sources::folder_node::{FolderPlaces, NodeFolders};

/// `{type:"kb", id, ws}`: `id` is a document or source id. Sent after the commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename = "kb")]
pub struct KbEvent {
    pub id: String,
    pub ws: String,
}

type Notify = dyn Fn(KbEvent) + Send + Sync;

/// The knowledge module's own state, shared by its routes, jobs and the index worker. Cheap to clone.
#[derive(Clone)]
pub struct Knowledge(Arc<Inner>);

pub struct Inner {
    /// Where knowledge is found ([`crate::port`]); the Hub's FTS index unless another is set.
    store: OnceLock<Arc<dyn crate::port::KnowledgeStore>>,
    notify: Arc<Notify>,
    pub(crate) loaders: Vec<Arc<dyn DocLoader>>,
    pub(crate) queue: OnceLock<mpsc::UnboundedSender<String>>,
    folder_policy: OnceLock<FolderPolicy>,
    /// The folder sources' Node side ([`Knowledge::set_folder_places`]).
    folder_nodes: Arc<NodeFolders>,
    /// Folder sources to scan soon (see [`Knowledge::request_scan`]).
    scan_requests: Mutex<Vec<String>>,
    pub(crate) scan_wake: tokio::sync::Notify,
}

impl std::ops::Deref for Knowledge {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Knowledge {
    /// `notify` forwards `{type:"kb"}` events to the SSE bus (integrator; tests collect them).
    /// Notes and folder files are always loadable; further loaders (uploads) come from
    /// [`Knowledge::with_loaders`].
    pub fn new(notify: impl Fn(KbEvent) + Send + Sync + 'static) -> Self {
        Self::with_loaders(notify, Vec::new())
    }

    pub fn with_loaders(
        notify: impl Fn(KbEvent) + Send + Sync + 'static,
        mut loaders: Vec<Arc<dyn DocLoader>>,
    ) -> Self {
        loaders.insert(0, Arc::new(crate::notes::NotesLoader));
        let folder_nodes = Arc::new(NodeFolders::default());
        loaders.insert(
            1,
            Arc::new(crate::sources::folder::FolderLoader {
                nodes: folder_nodes.clone(),
            }),
        );
        Self(Arc::new(Inner {
            store: OnceLock::new(),
            notify: Arc::new(notify),
            loaders,
            queue: OnceLock::new(),
            folder_policy: OnceLock::new(),
            folder_nodes,
            scan_requests: Mutex::new(Vec::new()),
            scan_wake: tokio::sync::Notify::new(),
        }))
    }

    /// Sets the knowledge store (once, while the Hub is wired, before anything searches).
    pub fn set_store(&self, store: Arc<dyn crate::port::KnowledgeStore>) {
        if self.store.set(store).is_err() {
            tracing::warn!("knowledge store already set");
        }
    }

    /// Where knowledge is searched.
    pub fn store(&self) -> &dyn crate::port::KnowledgeStore {
        self.store
            .get_or_init(|| Arc::new(crate::port::SqliteStore))
            .as_ref()
    }

    /// Asks the `kb.folder_scan` job ([`crate::spawn_scans`]) to scan a folder source now. Safe to
    /// call inside a write transaction: the scan claims the source in a write of its own, so it
    /// sees the committed state (and skips a source whose transaction rolled back).
    pub fn request_scan(&self, source_id: &str) {
        let mut q = self.scan_requests.lock().unwrap_or_else(|e| e.into_inner());
        if !q.iter().any(|x| x == source_id) {
            q.push(source_id.to_string());
        }
        drop(q);
        self.scan_wake.notify_one();
    }

    /// Takes the pending scan requests.
    pub fn take_scan_requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.scan_requests.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Events dropped: for tests and tools.
    pub fn bare() -> Self {
        Self::new(|_| {})
    }

    pub fn notify(&self, ws: &str, id: &str) {
        (self.notify)(KbEvent {
            id: id.into(),
            ws: ws.into(),
        });
    }

    /// Sets where `$HOME` and `STEVE_HOME` are for the folder checks. Without a call they
    /// come from the environment on first use. `false` if already set or used.
    pub fn set_folder_policy(&self, p: FolderPolicy) -> bool {
        self.folder_policy.set(p).is_ok()
    }

    pub fn folder_policy(&self) -> &FolderPolicy {
        self.folder_policy.get_or_init(FolderPolicy::from_env)
    }

    /// Reads folder sources through the Hub's router from now on: their Node scans and reads
    /// (wired only when the Hub's local Node is on). `false` if already set.
    pub fn set_folder_places(&self, p: Arc<dyn FolderPlaces>) -> bool {
        self.folder_nodes.set(p)
    }

    pub(crate) fn folder_nodes(&self) -> &NodeFolders {
        &self.folder_nodes
    }

    pub(crate) fn loader(&self, kind: &str, doc: &DocRow) -> Option<&Arc<dyn DocLoader>> {
        self.loaders
            .iter()
            .find(|l| l.kinds().contains(&kind) && l.accepts(doc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kb_event_json() {
        assert_eq!(
            serde_json::to_string(&KbEvent {
                id: "kd_1".into(),
                ws: "w".into()
            })
            .unwrap(),
            r#"{"type":"kb","id":"kd_1","ws":"w"}"#
        );
    }
}
