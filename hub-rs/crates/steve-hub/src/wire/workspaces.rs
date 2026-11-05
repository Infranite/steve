//! Where a project's, item's, session's or knowledge source's files are: which Node, and where on
//! it ([`Workspaces::placement_of`]). The Hub's own device (`hub`) is the local Node; any other
//! device is a paired remote Node, through N2's [`NodeFleet`]. A Node that is not there gives
//! [`PlacementError::Offline`] (`设备离线`), never a fallback to the Hub's own disk.
//!
//! The local Node's workspace faces open with it, only with `STEVE_NODE_MODE=local` and never on
//! a `STEVE_NO_RESUME` copy (D8): without it, the Hub's own device answers
//! [`PlacementError::LocalOff`] and callers keep their own path.
//!
//! The local Node's roots follow the Hub: they are built again whenever projects or knowledge
//! sources change ([`Workspaces::follow`]) and before a session is bound
//! ([`Workspaces::refresh_roots`]), so a new project needs no restart (only configured roots,
//! D11: no "anywhere under ~").
//!
//! Workspace operations that change things go through the ledger
//! ([`super::node_runtime::NodeRuntime::submit`]) of [`Placement::runtime`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::OptionalExtension;
use serde_json::Value;
use steve_node_api::*;
use steve_store::{Store, StoreError};

use super::node_binding::root_id;
use super::node_runtime::NodeRuntime;
use crate::nodes::NodeFleet;

/// The id of the Hub's own device (`projects.device_id`, `sessions.device_id`).
pub const HUB_DEVICE: &str = "hub";

/// The local Node: the Node itself (host bridges such as [`steve_node::LocalNode::set_roots`])
/// and its coordinator.
pub struct LocalFace {
    pub node: Arc<steve_node::LocalNode>,
    pub runtime: Arc<NodeRuntime>,
}

/// What to find the place of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject<'a> {
    Project(&'a str),
    Item(&'a str),
    Session(&'a str),
    KbSource(&'a str),
}

impl Subject<'_> {
    pub fn kind(&self) -> Kind {
        match self {
            Subject::Project(_) => Kind::Project,
            Subject::Item(_) => Kind::Item,
            Subject::Session(_) => Kind::Session,
            Subject::KbSource(_) => Kind::KbSource,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Project,
    Item,
    Session,
    KbSource,
}

/// Where on the Node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    /// A directory under one of the Node's roots (a project, a knowledge folder, a legacy cwd).
    Root(RootPath),
    /// A workspace the Node prepared (a bound session's).
    Workspace(WorkspaceRef),
}

pub struct Placement {
    pub node_id: NodeId,
    /// The Node's faces (workspace, discovery, control).
    pub node: Arc<dyn Node>,
    /// Its coordinator: the ledger operations go through it.
    pub runtime: Arc<NodeRuntime>,
    pub at: At,
}

#[derive(Debug)]
pub enum PlacementError {
    /// The device is unknown, not paired and active, or not linked now.
    Offline {
        device: String,
    },
    /// The Hub's own device, with the local Node off here (`STEVE_NODE_MODE` unset or off, or a
    /// `STEVE_NO_RESUME` copy).
    LocalOff,
    NotFound(Kind),
    /// It has no files anywhere yet (an item with no project, cwd or bound session).
    NoWorkspace,
    Store(StoreError),
}

impl std::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Offline { .. } => steve_i18n::t!("hub.workspaces.offline"),
            Self::LocalOff => steve_i18n::t!("hub.workspaces.local_off"),
            Self::NotFound(Kind::Project) => steve_i18n::t!("hub.not_found.project"),
            Self::NotFound(Kind::Item) => steve_i18n::t!("hub.not_found.item"),
            Self::NotFound(Kind::Session) => steve_i18n::t!("hub.not_found.session"),
            Self::NotFound(Kind::KbSource) => steve_i18n::t!("hub.not_found.kb_source"),
            Self::NoWorkspace => steve_i18n::t!("hub.workspaces.no_workspace"),
            Self::Store(e) => return write!(f, "{e}"),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for PlacementError {}

impl From<StoreError> for PlacementError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// The directories the Hub configures for the local Node besides projects and sessions.
pub fn configured(cfg: &crate::config::Config) -> Vec<PathBuf> {
    vec![cfg.scratch.clone(), cfg.worktrees.clone()]
}

pub struct Workspaces {
    store: Store,
    local: Option<LocalFace>,
    fleet: Option<Arc<NodeFleet>>,
    /// [`configured`], for building the local roots again.
    extra: Vec<PathBuf>,
    /// Held from reading the store to setting the roots, so a rebuild that read earlier never
    /// replaces a later one ([`Workspaces::follow`] and a session's bind both rebuild).
    rebuild: Mutex<()>,
}

/// Where a thing is, as the store says, before any Node is asked.
enum Place {
    Device(String, At),
    Bound(String, WorkspaceRef),
}

impl Workspaces {
    pub fn new(
        store: Store,
        local: Option<LocalFace>,
        fleet: Option<Arc<NodeFleet>>,
        extra: Vec<PathBuf>,
    ) -> Arc<Workspaces> {
        Arc::new(Workspaces {
            store,
            local,
            fleet,
            extra,
            rebuild: Mutex::new(()),
        })
    }

    /// The Node and the place of `what`.
    pub fn placement_of(&self, what: Subject<'_>) -> Result<Placement, PlacementError> {
        let (place, workspace) = self.place(what)?;
        let (device, at) = match place {
            Place::Device(d, at) => (d, at),
            Place::Bound(node, w) => (node, At::Workspace(w)),
        };
        self.on(&device, at, &workspace)
    }

    /// `at` on the Node of a device: the local one for `hub` (and its node id `local`), else a
    /// paired, active and linked remote Node that may get `workspace`'s work
    /// ([`super::node_binding::node_takes`]: paired by an owner of the workspace).
    fn on(&self, device: &str, at: At, workspace: &str) -> Result<Placement, PlacementError> {
        let offline = || PlacementError::Offline {
            device: device.into(),
        };
        if device == HUB_DEVICE || self.is_local(device) {
            let l = self.local.as_ref().ok_or(PlacementError::LocalOff)?;
            return Ok(Placement {
                node_id: l.runtime.node_id().clone(),
                node: l.node.clone(),
                runtime: l.runtime.clone(),
                at,
            });
        }
        if device == "local" {
            return Err(PlacementError::LocalOff);
        }
        let (Some(fleet), Ok(id)) = (&self.fleet, NodeId::new(device)) else {
            return Err(offline());
        };
        let takes = self
            .store
            .reader(|c| super::node_binding::node_takes(c, id.as_str(), workspace))?;
        let remote = fleet.remote(&id);
        if !takes || remote.generation().is_none() {
            return Err(offline());
        }
        let runtime = fleet.start(&id).ok_or_else(offline)?;
        Ok(Placement {
            node_id: id,
            node: remote,
            runtime,
            at,
        })
    }

    fn is_local(&self, node: &str) -> bool {
        self.local
            .as_ref()
            .is_some_and(|l| l.runtime.node_id().as_str() == node)
    }

    /// The place of `what` and the workspace it belongs to.
    fn place(&self, what: Subject<'_>) -> Result<(Place, String), PlacementError> {
        let (found, workspace) = self.store.reader(|c| {
            let (table, id) = match what {
                Subject::Project(id) => ("projects", id),
                Subject::Session(id) => ("sessions", id),
                Subject::Item(id) => ("items", id),
                Subject::KbSource(id) => ("kb_sources", id),
            };
            let workspace: Option<String> = c
                .query_row(
                    &format!("SELECT workspace_id FROM {table} WHERE id=?"),
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            let found = match what {
                Subject::Project(id) => project_in(c, id)?,
                Subject::Session(id) => session_in(c, id)?,
                Subject::Item(id) => item_in(c, id)?,
                Subject::KbSource(id) => kb_source_in(c, id)?,
            };
            Ok::<_, StoreError>((found, workspace))
        })?;
        match (found, workspace) {
            (Found::Missing, _) | (_, None) => Err(PlacementError::NotFound(what.kind())),
            (Found::Nowhere, _) => Err(PlacementError::NoWorkspace),
            (Found::At(p), Some(w)) => Ok((p, w)),
        }
    }

    /// Builds the local Node's roots again from the store (nothing without a local Node).
    pub fn refresh_roots(&self) -> Result<(), StoreError> {
        if let Some(l) = &self.local {
            let _one = self.rebuild.lock().unwrap_or_else(|e| e.into_inner());
            l.node
                .set_roots(super::node_binding::roots(&self.store, &self.extra)?);
        }
        Ok(())
    }

    /// Keeps the local roots in step with projects and knowledge sources: `events` are the bus's
    /// `project` and `kb` events. A burst is one rebuild.
    pub async fn follow(self: Arc<Self>, mut events: tokio::sync::mpsc::UnboundedReceiver<Value>) {
        while events.recv().await.is_some() {
            while events.try_recv().is_ok() {}
            let me = self.clone();
            match tokio::task::spawn_blocking(move || me.refresh_roots()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("local Node roots: {e}"),
                Err(e) => tracing::error!("local Node roots: {e}"),
            }
        }
    }
}

enum Found {
    Missing,
    Nowhere,
    At(Place),
}

/// A directory of the Hub's own device as a root path (the local roots are named by path).
fn hub_dir(path: &str) -> Option<At> {
    Some(At::Root(RootPath {
        root: root_id(Path::new(path))?,
        rel: RelPath::new("").ok()?,
    }))
}

/// A directory on a remote device, as `<root id>/<rel>` (its owner names its roots; an absolute
/// path means nothing there).
fn remote_dir(path: &str) -> Option<At> {
    if path.starts_with('/') {
        return None;
    }
    let (root, rel) = path.split_once('/').unwrap_or((path, ""));
    Some(At::Root(RootPath {
        root: RootId::new(root).ok()?,
        rel: RelPath::new(rel).ok()?,
    }))
}

fn dir_on(device: String, path: &str) -> Found {
    let at = if device == HUB_DEVICE {
        hub_dir(path)
    } else {
        remote_dir(path)
    };
    match at {
        Some(at) => Found::At(Place::Device(device, at)),
        None => Found::Nowhere,
    }
}

fn project_in(c: &rusqlite::Connection, id: &str) -> Result<Found, StoreError> {
    let row: Option<(String, String, bool)> = c
        .query_row(
            "SELECT device_id, path, archived FROM projects WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(match row {
        None => Found::Missing,
        // an archived project's directory is no root any more
        Some((_, _, true)) => Found::Nowhere,
        Some((device, path, false)) => dir_on(device, &path),
    })
}

/// A session bound to a Node: its workspace there. Else its directory on its device.
fn session_in(c: &rusqlite::Connection, id: &str) -> Result<Found, StoreError> {
    if let Some(p) = bound_in(c, id)? {
        return Ok(Found::At(p));
    }
    let row: Option<(String, String, bool)> = c
        .query_row(
            "SELECT s.device_id, s.cwd, COALESCE(p.archived, 0) FROM sessions s
              LEFT JOIN items i ON i.id=s.item_id LEFT JOIN projects p ON p.id=i.project_id
              WHERE s.id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(match row {
        None => Found::Missing,
        // an archived project's directories are no roots any more, its items' included
        Some((_, cwd, archived)) if cwd.is_empty() || archived => Found::Nowhere,
        Some((device, cwd, _)) => dir_on(device, &cwd),
    })
}

fn bound_in(c: &rusqlite::Connection, session: &str) -> Result<Option<Place>, StoreError> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT node_id, workspace_ref FROM runtime_sessions WHERE session_id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(node, w)| {
        let w: WorkspaceRef = serde_json::from_str(&w).ok()?;
        (w.node.as_str() == node).then_some(Place::Bound(node, w))
    }))
}

/// An item's latest bound session's workspace; else its own directory (on the Hub); else its
/// project's. An archived project's item has no directory any more (as [`project_in`]).
fn item_in(c: &rusqlite::Connection, id: &str) -> Result<Found, StoreError> {
    let row: Option<(Option<String>, Option<String>, bool)> = c
        .query_row(
            "SELECT i.cwd, i.project_id, COALESCE(p.archived, 0) FROM items i
              LEFT JOIN projects p ON p.id=i.project_id WHERE i.id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((cwd, project, archived)) = row else {
        return Ok(Found::Missing);
    };
    let latest: Option<String> = c
        .query_row(
            "SELECT s.id FROM sessions s JOIN runtime_sessions r ON r.session_id=s.id
              WHERE s.item_id=? ORDER BY s.created DESC, s.rowid DESC LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(s) = latest
        && let Some(p) = bound_in(c, &s)?
    {
        return Ok(Found::At(p));
    }
    if archived {
        return Ok(Found::Nowhere);
    }
    if let Some(cwd) = cwd.filter(|p| !p.is_empty()) {
        return Ok(dir_on(HUB_DEVICE.into(), &cwd));
    }
    match project {
        Some(p) => project_in(c, &p),
        None => Ok(Found::Nowhere),
    }
}

/// A knowledge folder: a read-only root of the Hub's own device.
fn kb_source_in(c: &rusqlite::Connection, id: &str) -> Result<Found, StoreError> {
    let row: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT kind, json_extract(config,'$.path') FROM kb_sources WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        None => Found::Missing,
        Some((kind, Some(path))) if kind == "folder" => dir_on(HUB_DEVICE.into(), &path),
        Some(_) => Found::Nowhere,
    })
}

#[cfg(test)]
mod tests {
    use super::super::node_tools::tests::Ends;
    use super::*;
    use std::time::Duration;

    struct T {
        dir: tempfile::TempDir,
        store: Store,
        node: Arc<steve_node::LocalNode>,
        ws: Arc<Workspaces>,
    }

    fn exec(store: &Store, sql: &str) {
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
    }

    /// A store and a local Node opened on boot's roots (none but scratch), with no harness;
    /// `remote`: remote Nodes are listened for (a fleet with no Node linked).
    fn local(remote: bool) -> T {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let extra = vec![dir.path().join("scratch")];
        std::fs::create_dir_all(&extra[0]).unwrap();
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                node: NodeId::new("local").unwrap(),
                home: dir.path().join("node"),
                acp: steve_acp::AcpConfig {
                    adapters: Default::default(),
                    node: "node".into(),
                    claude_executable: "/usr/bin/false".into(),
                    steve_home: dir.path().join("steve"),
                    codex_home: dir.path().join("codex"),
                    user_home: dir.path().to_path_buf(),
                    version: "test".into(),
                    idle_release: Duration::from_secs(600),
                    retry_delays: vec![],
                },
                roots: super::super::node_binding::roots(&store, &extra).unwrap(),
                capacity: Capacity {
                    sessions: 0,
                    jobs: 0,
                    execs: 0,
                },
                storage: Default::default(),
            })
            .unwrap(),
        );
        let runtime = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Ends));
        let fleet = remote.then(|| {
            NodeFleet::new(
                store.clone(),
                dir.path().to_path_buf(),
                Some(Arc::new(Ends)),
                None,
            )
        });
        let ws = Workspaces::new(
            store.clone(),
            Some(LocalFace {
                node: node.clone(),
                runtime,
            }),
            fleet,
            extra,
        );
        T {
            dir,
            store,
            node,
            ws,
        }
    }

    fn project(t: &T, id: &str, device: &str, path: &str) {
        exec(
            &t.store,
            &format!(
                "INSERT INTO projects(id,workspace_id,name,path,kind,device_id,created,updated) VALUES('{id}','default','{id}','{path}','dir','{device}',1,1)"
            ),
        );
    }

    fn root_of(p: &Placement) -> RootPath {
        match &p.at {
            At::Root(r) => r.clone(),
            other => panic!("not a root: {other:?}"),
        }
    }

    async fn prepare(node: &steve_node::LocalNode, op: &str, at: RootPath) -> ApiResult<Receipt> {
        let now = steve_store::now();
        node.prepare_workspace(Command {
            id: OperationId::new(op).unwrap(),
            issued_at: Timestamp(now),
            accept_before: Timestamp(now + 60_000),
            grant: GrantId::new("hub").unwrap(),
            body: Prepare {
                resource: format!("project:{op}"),
                kind: PrepareKind::Existing { dir: at },
            },
        })
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_hub_device_is_the_local_node_and_any_other_is_offline() {
        let t = local(true);
        let p = t.dir.path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        project(&t, "p1", "hub", p.to_str().unwrap());
        project(&t, "p2", "n_far", "work/repo");
        project(&t, "p3", "n_gone", "work");
        exec(
            &t.store,
            "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','sub_u','u','owner',1);
             INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_far','u','far',zeroblob(32),'ff','active',1);",
        );
        let local = t.ws.placement_of(Subject::Project("p1")).unwrap();
        assert_eq!(local.node_id.as_str(), "local");
        assert_eq!(local.node.id().as_str(), "local");
        assert_eq!(root_of(&local).root, root_id(&p).unwrap());
        // a paired Node that is not linked now, and a device nobody paired: offline, never the Hub
        for p in ["p2", "p3"] {
            let e = t.ws.placement_of(Subject::Project(p)).err().unwrap();
            assert!(matches!(e, PlacementError::Offline { .. }), "{p}: {e:?}");
            assert_eq!(e.to_string(), "设备离线");
        }
        assert_eq!(
            steve_i18n::scope("en", async {
                PlacementError::Offline { device: "x".into() }.to_string()
            })
            .await,
            "Device offline"
        );
        assert!(matches!(
            t.ws.placement_of(Subject::Project("nope")),
            Err(PlacementError::NotFound(Kind::Project))
        ));
        // without the local Node (STEVE_NODE_MODE off) the Hub's own device has no faces
        let off = Workspaces::new(t.store.clone(), None, None, vec![]);
        assert!(matches!(
            off.placement_of(Subject::Project("p1")),
            Err(PlacementError::LocalOff)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sessions_items_and_knowledge_folders_have_their_place() {
        let t = local(false);
        let cwd = t.dir.path().join("legacy");
        let kb = t.dir.path().join("wiki");
        exec(
            &t.store,
            &format!(
                "INSERT INTO items(id,workspace_id,no,title,status,stage,cwd,created,updated) VALUES ('i1','default',1,'t','run','执行','{c}',1,1),('i2','default',2,'t','run','执行',NULL,1,1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','{c}','active',1,1),('s2','default','i1','claude-code','','active',2,2);
                 INSERT INTO kb_sources(id,workspace_id,kind,name,config,created_by,created,updated) VALUES ('k1','default','folder','wiki','{{\"path\":\"{k}\"}}','u',1,1),('k2','default','notes','n','{{}}','u',1,1);",
                c = cwd.display(),
                k = kb.display(),
            ),
        );
        // a legacy session: its directory
        let s1 = t.ws.placement_of(Subject::Session("s1")).unwrap();
        assert_eq!(root_of(&s1).root, root_id(&cwd).unwrap());
        assert!(matches!(
            t.ws.placement_of(Subject::Session("s2")),
            Err(PlacementError::NoWorkspace)
        ));
        // the item's directory, until one of its sessions is bound: then that workspace
        assert_eq!(
            root_of(&t.ws.placement_of(Subject::Item("i1")).unwrap()).root,
            root_id(&cwd).unwrap()
        );
        let w = WorkspaceRef {
            node: NodeId::new("local").unwrap(),
            id: WorkspaceId::new(derived_id("w", "item:i1")).unwrap(),
        };
        exec(
            &t.store,
            &format!(
                "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES ('s2','local','{}','s2','r','ready',0);",
                serde_json::to_string(&w).unwrap()
            ),
        );
        for what in [Subject::Item("i1"), Subject::Session("s2")] {
            assert_eq!(
                t.ws.placement_of(what).unwrap().at,
                At::Workspace(w.clone())
            );
        }
        assert!(matches!(
            t.ws.placement_of(Subject::Item("i2")),
            Err(PlacementError::NoWorkspace)
        ));
        // a knowledge folder is a read-only root of the local Node
        let k1 = t.ws.placement_of(Subject::KbSource("k1")).unwrap();
        assert_eq!(root_of(&k1).root, root_id(&kb).unwrap());
        assert!(matches!(
            t.ws.placement_of(Subject::KbSource("k2")),
            Err(PlacementError::NoWorkspace)
        ));
        std::fs::create_dir_all(&kb).unwrap();
        t.ws.refresh_roots().unwrap();
        let caps = t.node.probe(false).await.unwrap();
        let info = caps
            .roots
            .iter()
            .find(|r| r.id == root_of(&k1).root)
            .unwrap();
        assert_eq!(info.access, RootAccess::ReadOnly);
        let e = prepare(&t.node, "op_kb", root_of(&k1)).await.unwrap_err();
        assert_eq!(
            e.code,
            ErrorCode::Denied,
            "a read-only root takes no workspace"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_project_added_after_boot_is_prepared_without_a_restart() {
        let t = local(false);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(t.ws.clone().follow(rx));
        let p = t.dir.path().join("later");
        std::fs::create_dir_all(&p).unwrap();
        project(&t, "p9", "hub", p.to_str().unwrap());
        let at = root_of(&t.ws.placement_of(Subject::Project("p9")).unwrap());
        // boot's roots do not have it
        assert_eq!(
            prepare(&t.node, "op_a", at.clone()).await.unwrap_err().code,
            ErrorCode::Denied
        );
        // the project event rebuilds them
        tx.send(serde_json::json!({"type": "project", "id": "p9"}))
            .unwrap();
        let mut taken = false;
        for i in 0..200 {
            if prepare(&t.node, &format!("op_b{i}"), at.clone())
                .await
                .is_ok()
            {
                taken = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(taken, "the new project's root was never allowed");
        // a dir project's item and session work in the project's directory (core's prepare_cwd)
        exec(
            &t.store,
            &format!(
                "INSERT INTO items(id,workspace_id,no,title,status,stage,cwd,project_id,created,updated) VALUES ('i9','default',9,'t','run','执行','{c}','p9',1,1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s9','default','i9','claude-code','{c}','active',1,1);",
                c = p.display()
            ),
        );
        t.ws.refresh_roots().unwrap();
        assert!(matches!(
            super::super::node_binding::binding_for(&t.store, "s9", None, &|p| t.node.allows(p))
                .unwrap()
                .prepare,
            PrepareKind::Existing { .. }
        ));
        // archived: its root goes, and its items' and sessions' directories with it
        exec(&t.store, "UPDATE projects SET archived=1 WHERE id='p9'");
        t.ws.refresh_roots().unwrap();
        assert_eq!(
            prepare(&t.node, "op_c", at).await.unwrap_err().code,
            ErrorCode::Denied
        );
        for what in [
            Subject::Project("p9"),
            Subject::Item("i9"),
            Subject::Session("s9"),
        ] {
            assert!(
                matches!(t.ws.placement_of(what), Err(PlacementError::NoWorkspace)),
                "{what:?}"
            );
        }
        assert!(
            super::super::node_binding::binding_for(&t.store, "s9", None, &|p| t.node.allows(p))
                .is_none()
        );
    }

    struct Quiet;
    #[async_trait::async_trait]
    impl steve_node_wire::Handler for Quiet {
        async fn handle(&self, method: &str, _: Value) -> ApiResult<Value> {
            Err(steve_node_wire::conn::unsupported(method))
        }
    }

    /// Links the remote Node `id` of `fleet` to a peer that answers nothing; keep the peer.
    fn link(fleet: &NodeFleet, id: &str) -> Arc<steve_node_wire::Conn> {
        let (h, n) = steve_node_wire::testkit::pair();
        let opts = |side| steve_node_wire::ConnOptions {
            side,
            ..Default::default()
        };
        let hub = steve_node_wire::Conn::spawn(h.sink, h.stream, 1, Arc::new(Quiet), opts("h"));
        let peer = steve_node_wire::Conn::spawn(n.sink, n.stream, 1, Arc::new(Quiet), opts("n"));
        assert!(fleet.remote(&NodeId::new(id).unwrap()).attach(hub, false));
        peer
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remote_node_gets_only_work_of_a_workspace_its_pairer_owns() {
        let t = local(false);
        let fleet = NodeFleet::new(
            t.store.clone(),
            t.dir.path().to_path_buf(),
            Some(Arc::new(Ends)),
            None,
        );
        let ws = Workspaces::new(t.store.clone(), None, Some(fleet.clone()), vec![]);
        exec(
            &t.store,
            "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','sub_u','u','owner',1),('m','default','sub_m','m','member',1);
             INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1),('default','m','member',1);
             INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_u','u','u',zeroblob(32),'fu','active',1),('n_m','m','m',zeroblob(32),'fm','active',1);
",
        );
        project(&t, "pu", "n_u", "work/repo");
        project(&t, "pm", "n_m", "work/repo");
        exec(
            &t.store,
            "INSERT INTO items(id,workspace_id,no,title,status,stage,project_id,created,updated) VALUES ('iu','default',1,'t','run','执行','pu',1,1),('im','default',2,'t','run','执行','pm',1,1);",
        );
        let _peers = [link(&fleet, "n_u"), link(&fleet, "n_m")];
        // the owner's linked Node has the owner's workspace's project and items
        for what in [Subject::Project("pu"), Subject::Item("iu")] {
            let p = ws.placement_of(what).unwrap();
            assert_eq!(p.node_id.as_str(), "n_u");
        }
        // a Node a plain member paired is offline for it, linked and active as it is
        for what in [Subject::Project("pm"), Subject::Item("im")] {
            let e = ws.placement_of(what).err().unwrap();
            assert!(
                matches!(e, PlacementError::Offline { .. }),
                "{what:?}: {e:?}"
            );
        }
        // and so is the owner's once paused
        exec(
            &t.store,
            "UPDATE nodes SET state='paused' WHERE node_id='n_u'",
        );
        assert!(matches!(
            ws.placement_of(Subject::Project("pu")),
            Err(PlacementError::Offline { .. })
        ));
    }
}
