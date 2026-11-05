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
use std::sync::Arc;

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
        })
    }

    /// The Node and the place of `what`.
    pub fn placement_of(&self, what: Subject<'_>) -> Result<Placement, PlacementError> {
        let (device, at) = match self.place(what)? {
            Place::Device(d, at) => (d, at),
            Place::Bound(node, w) => (node, At::Workspace(w)),
        };
        self.on(&device, at)
    }

    /// `at` on the Node of a device: the local one for `hub` (and its node id `local`), else a
    /// paired, active and linked remote Node.
    fn on(&self, device: &str, at: At) -> Result<Placement, PlacementError> {
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
        let active = self.store.reader(|c| {
            Ok::<_, StoreError>(
                steve_store::nodes::get(c, id.as_str())?.is_some_and(|n| n.state == "active"),
            )
        })?;
        let remote = fleet.remote(&id);
        if !active || remote.generation().is_none() {
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

    fn place(&self, what: Subject<'_>) -> Result<Place, PlacementError> {
        let found = self.store.reader(|c| {
            Ok::<_, StoreError>(match what {
                Subject::Project(id) => project_in(c, id)?,
                Subject::Session(id) => session_in(c, id)?,
                Subject::Item(id) => item_in(c, id)?,
                Subject::KbSource(id) => kb_source_in(c, id)?,
            })
        })?;
        match found {
            Found::Missing => Err(PlacementError::NotFound(what.kind())),
            Found::Nowhere => Err(PlacementError::NoWorkspace),
            Found::At(p) => Ok(p),
        }
    }

    /// Builds the local Node's roots again from the store (nothing without a local Node).
    pub fn refresh_roots(&self) -> Result<(), StoreError> {
        if let Some(l) = &self.local {
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
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT device_id, cwd FROM sessions WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        None => Found::Missing,
        Some((_, cwd)) if cwd.is_empty() => Found::Nowhere,
        Some((device, cwd)) => dir_on(device, &cwd),
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
/// project's.
fn item_in(c: &rusqlite::Connection, id: &str) -> Result<Found, StoreError> {
    let row: Option<(Option<String>, Option<String>)> = c
        .query_row("SELECT cwd, project_id FROM items WHERE id=?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((cwd, project)) = row else {
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
        // archived: its root goes
        exec(&t.store, "UPDATE projects SET archived=1 WHERE id='p9'");
        t.ws.refresh_roots().unwrap();
        assert_eq!(
            prepare(&t.node, "op_c", at).await.unwrap_err().code,
            ErrorCode::Denied
        );
        assert!(matches!(
            t.ws.placement_of(Subject::Project("p9")),
            Err(PlacementError::NoWorkspace)
        ));
    }
}
