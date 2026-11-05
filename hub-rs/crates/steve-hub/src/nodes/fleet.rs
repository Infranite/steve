//! One coordinator per paired Node, each over its own [`RemoteNode`]: the Hub's routing
//! ([`crate::wire::core::RoutedRuntime`]) finds where a session's commands go by
//! `runtime_sessions.node_id` and [`NodeFleet::start`]. Each coordinator leases only its own
//! Node's commands, so they share the store with each other and with the local Node's.
//!
//! The ports a Node's link may use (its event sink, Steve's tools, attachments) are built here for
//! that one Node and only ever handed to the link through [`super::guard`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use steve_node_api::*;
use steve_node_wire::RemoteNode;
use steve_store::Store;

use crate::wire::node_files::NodeFiles;
use crate::wire::node_runtime::{NodeDomain, NodeRuntime, Sink};
use crate::wire::node_tools::{NodeTools, Service};

struct Entry {
    remote: Arc<RemoteNode>,
    runtime: Option<Arc<NodeRuntime>>,
    tools: Option<Arc<dyn HubTools>>,
    /// The protocol of the Node's latest link since the Hub started (`None`: not linked yet).
    proto: Option<(u16, u16)>,
}

pub struct NodeFleet {
    store: Store,
    data_dir: PathBuf,
    domain: Option<Arc<dyn NodeDomain>>,
    /// Steve's tools; the Hub installs them once they exist ([`Self::set_service`]).
    service: OnceLock<Arc<dyn Service>>,
    nodes: Mutex<HashMap<String, Entry>>,
    /// The Hub's release hold, for every coordinator (and the ones started later).
    holding: AtomicBool,
    /// An attempt's deadline for the coordinators started from now on (ms; 0: their default).
    turn_limit: AtomicI64,
    /// How long a session may sit idle before it is let go, for the coordinators started from
    /// now on (ms; -1: their default).
    idle_release: AtomicI64,
}

/// Events of a Node no coordinator runs for yet: refused `offline`, so they stay in its journal.
struct NoRuntime;

#[async_trait]
impl EventSink for NoRuntime {
    async fn commit(&self, _: EventBatch) -> ApiResult<EventAck> {
        Err(ApiError::new(
            ErrorCode::Offline,
            "no coordinator for this node yet",
        ))
    }
}

struct NoTools(ApiError);

#[async_trait]
impl HubTools for NoTools {
    async fn catalog(&self, _: ExecutionRef) -> ApiResult<Catalog> {
        Err(self.0.clone())
    }
    async fn call(&self, _: ToolCall) -> ApiResult<ToolResult> {
        Err(self.0.clone())
    }
    async fn call_status(&self, _: ToolKey) -> ApiResult<ToolResult> {
        Err(self.0.clone())
    }
    async fn cancel_call(&self, _: ToolKey) -> ApiResult<ToolResult> {
        Err(self.0.clone())
    }
}

impl NodeFleet {
    /// `domain`: the business side the coordinators report to (the Hub core after D5);
    /// `service`: Steve's tools (the ToolService). Without them a Node links but runs nothing.
    pub fn new(
        store: Store,
        data_dir: PathBuf,
        domain: Option<Arc<dyn NodeDomain>>,
        service: Option<Arc<dyn Service>>,
    ) -> Arc<NodeFleet> {
        let fleet = Arc::new(NodeFleet {
            store,
            data_dir,
            domain,
            service: OnceLock::new(),
            nodes: Mutex::new(HashMap::new()),
            holding: AtomicBool::new(false),
            turn_limit: AtomicI64::new(0),
            idle_release: AtomicI64::new(-1),
        });
        if let Some(s) = service {
            fleet.set_service(s);
        }
        fleet
    }

    /// Steve's tools for every Node's link, including the ones opened before.
    pub fn set_service(&self, service: Arc<dyn Service>) {
        let _ = self.service.set(service);
    }

    pub fn set_turn_limit(&self, ms: i64) {
        self.turn_limit.store(ms.max(1), Ordering::Relaxed);
    }

    pub fn set_idle_release(&self, ms: i64) {
        self.idle_release.store(ms.max(0), Ordering::Relaxed);
    }

    /// The Hub's release hold on every coordinator.
    pub fn hold(&self, on: bool) {
        self.holding.store(on, Ordering::SeqCst);
        for rt in self.runtimes() {
            rt.hold(on);
        }
    }

    /// The Hub's hold only: a Node its owner paused does not hold the Hub.
    pub fn is_holding(&self) -> bool {
        self.holding.load(Ordering::SeqCst)
    }

    /// Turns in flight on all remote Nodes.
    pub fn running_turns(&self) -> usize {
        self.runtimes().iter().map(|rt| rt.running_turns()).sum()
    }

    /// The coordinators started so far.
    pub fn runtimes(&self) -> Vec<Arc<NodeRuntime>> {
        self.nodes
            .lock()
            .unwrap()
            .values()
            .filter_map(|e| e.runtime.clone())
            .collect()
    }

    fn entry<T>(&self, node: &NodeId, f: impl FnOnce(&mut Entry) -> T) -> T {
        let mut nodes = self.nodes.lock().unwrap();
        let e = nodes.entry(node.to_string()).or_insert_with(|| Entry {
            remote: RemoteNode::new(node.clone()),
            runtime: None,
            tools: None,
            proto: None,
        });
        f(e)
    }

    /// The Hub's handle on the Node, linked or not.
    pub fn remote(&self, node: &NodeId) -> Arc<RemoteNode> {
        self.entry(node, |e| e.remote.clone())
    }

    /// Records the protocol the Node's hello said, for each link it opens.
    pub fn linked(&self, node: &NodeId, proto: (u16, u16)) {
        self.entry(node, |e| e.proto = Some(proto));
    }

    /// Whether the Node is known to speak `since` or later: its latest link said so. A Node not
    /// linked since the Hub started counts as not, so what a minor added is never sent to it.
    pub fn speaks(&self, node: &NodeId, since: (u16, u16)) -> bool {
        self.speaks_known(node, since).unwrap_or(false)
    }

    /// [`Self::speaks`], telling "not known yet" (`None`: not linked since the Hub started) from
    /// "its hello said an older protocol" (`Some(false)`).
    pub fn speaks_known(&self, node: &NodeId, since: (u16, u16)) -> Option<bool> {
        self.entry(node, |e| e.proto.map(|p| p.0 == since.0 && p >= since))
    }

    /// Starts the Node's coordinator (once).
    pub fn start(&self, node: &NodeId) -> Option<Arc<NodeRuntime>> {
        let domain = self.domain.clone()?;
        let (rt, fresh) = self.entry(node, |e| match &e.runtime {
            Some(rt) => (rt.clone(), false),
            None => {
                let rt = NodeRuntime::new(self.store.clone(), e.remote.clone(), domain);
                // what the Node replays passes the link's guard, as what it pushes does
                rt.over_link(
                    super::guard::replay_check(self.store.clone(), node.clone()),
                    steve_node_wire::envelope::MAX_FRAME as u32,
                );
                let ms = self.turn_limit.load(Ordering::Relaxed);
                if ms > 0 {
                    rt.set_turn_limit(ms);
                }
                let idle = self.idle_release.load(Ordering::Relaxed);
                if idle >= 0 {
                    rt.set_idle_release(idle);
                }
                rt.hold(self.holding.load(Ordering::SeqCst));
                e.runtime = Some(rt.clone());
                (rt, true)
            }
        });
        if fresh {
            rt.start();
        }
        Some(rt)
    }

    /// Where a session bound to `node` runs.
    pub fn runtime_for(&self, node: &str) -> Option<Arc<NodeRuntime>> {
        self.nodes
            .lock()
            .unwrap()
            .get(node)
            .and_then(|e| e.runtime.clone())
    }

    pub fn nodes(&self) -> Vec<String> {
        self.nodes.lock().unwrap().keys().cloned().collect()
    }

    pub(crate) fn sink_for(&self, node: &NodeId) -> Arc<dyn EventSink> {
        match self.runtime_for(node.as_str()) {
            Some(rt) => Arc::new(Sink(Arc::downgrade(&rt))),
            None => Arc::new(NoRuntime),
        }
    }

    pub(crate) fn tools_for(&self, node: &NodeId) -> Arc<dyn HubTools> {
        if let Some(t) = self.entry(node, |e| e.tools.clone()) {
            return t;
        }
        let home = self.data_dir.join("nodes").join(node.as_str());
        let t: Arc<dyn HubTools> = match NodeTools::open(self.store.clone(), node.clone(), &home) {
            Ok(t) => match self.service.get() {
                Some(s) => {
                    let _ = t.install(s.clone());
                    t
                }
                // the Hub has not finished starting: refused for now, the Node calls again
                None => {
                    return Arc::new(NoTools(ApiError::new(
                        ErrorCode::Offline,
                        "the Hub's tools are not ready yet",
                    )));
                }
            },
            Err(e) => Arc::new(NoTools(e)),
        };
        self.entry(node, |e| e.tools.get_or_insert(t).clone())
    }

    pub(crate) fn files_for(&self, node: &NodeId) -> Arc<dyn HubFiles> {
        Arc::new(NodeFiles::new(
            self.store.clone(),
            node.clone(),
            self.data_dir.clone(),
        ))
    }
}
