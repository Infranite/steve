//! Removing a paired Node from its machine (#325): the owner asks from the Node's page, the Hub
//! asks the Node over its own link, the Node removes itself.
//!
//! A Node can be asked when it is linked and its probe says [`FEATURE_NODE_REMOVE`] (a Node
//! installed as a service). Any other — one offline, one run by hand — is removed on its machine with
//! [`MANUAL_COMMAND`], which Devices shows instead.
//!
//! One removal, `node_removals` row by row (each phase written before it is taken):
//!
//! 1. `requested` → `draining`: the Hub stops giving the Node new turns and sends `node.remove`
//!    with the grace period (`STEVE_NODE_REMOVE_GRACE_SECS`, default 10 min). The Node takes no
//!    new work, lets what runs end, and past the grace period cancels it (`cancelling`).
//! 2. `ready`: nothing runs on the Node; nothing is uninstalled yet.
//! 3. `confirming`: the Hub confirms (`node.remove_commit`). The Node answers `removing`, drops
//!    its link, and a helper outside its service stops and uninstalls the service and deletes the
//!    Node's home.
//! 4. `removed`: the Hub unpairs the Node (as "Unpair" does: it can never link again).
//!
//! A Node that does not get ready in time, goes away, or never says whether it took the
//! confirmation ends `unconfirmed`: it stays paired and Devices shows the command to run there.
//! A lost answer is asked about (`node.remove_status`), never sent again. After a Hub restart an
//! unfinished removal is settled from the Node's state, never run on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_node_api::*;
use steve_store::Store;
use steve_store::nodes::{self as store_nodes, NodeRow, REMOVAL_DONE, RemovalRow};

use super::{NodeService, now};

/// What to run on a Node's machine to remove it there by hand (a Node offline, of an earlier
/// release, or one that did not confirm): stop and uninstall its service with the Steve CLI (on the
/// `PATH`, else the one its setup left in its home), then delete its home. For a Node set up with
/// the default home (`~/.steve-node`).
pub const MANUAL_COMMAND: &str = r#"S=$(command -v steve || ls ~/.steve-node/releases/*/bin/steve | tail -n 1) && "$S" node stop && "$S" node uninstall && rm -rf ~/.steve-node"#;

/// What the orchestration needs of the fleet, so tests can stand in.
#[async_trait]
pub trait Fleet: Send + Sync {
    fn discovery(&self, node: &NodeId) -> Arc<dyn Discovery>;
    fn linked(&self, node: &str) -> bool;
    /// Stop (or start again) giving the Node new turns.
    fn hold(&self, node: &str, on: bool);
    /// Unpair `node` for its owner `by`, as Devices' "Unpair" does; whether it was.
    async fn unpair(&self, node: &str, by: &str) -> bool;
}

/// The real fleet: the Node service's links, coordinators and store.
pub struct LiveFleet {
    pub svc: Weak<NodeService>,
}

#[async_trait]
impl Fleet for LiveFleet {
    fn discovery(&self, node: &NodeId) -> Arc<dyn Discovery> {
        match self.svc.upgrade() {
            Some(svc) => svc.fleet.remote(node),
            None => steve_node_wire::RemoteNode::new(node.clone()),
        }
    }
    fn linked(&self, node: &str) -> bool {
        self.svc
            .upgrade()
            .is_some_and(|svc| svc.registry.current(node).is_some())
    }
    fn hold(&self, node: &str, on: bool) {
        if let Some(svc) = self.svc.upgrade()
            && let Some(rt) = svc.fleet.runtime_for(node)
        {
            rt.set_paused(on);
        }
    }
    async fn unpair(&self, node: &str, by: &str) -> bool {
        let Some(svc) = self.svc.upgrade() else {
            return false;
        };
        let (store, n, by) = (svc.store.clone(), node.to_string(), by.to_string());
        let done = tokio::task::spawn_blocking(move || {
            store.transaction(|tx| {
                let ok = store_nodes::revoke(tx, &n, &by, now())?;
                if ok {
                    store_nodes::audit(
                        tx,
                        Some(&n),
                        Some(&by),
                        "revoke",
                        Some("removed from its machine"),
                        now(),
                    )?;
                }
                Ok::<_, steve_store::StoreError>(ok)
            })
        })
        .await;
        let ok = matches!(done, Ok(Ok(true)));
        if ok {
            svc.revoked(node).await;
        } else {
            tracing::error!(node, "unpairing the removed node: {done:?}");
        }
        ok
    }
}

#[derive(Debug, Clone)]
pub struct Timing {
    pub poll: Duration,
    /// How long the Node lets running turns, jobs and commands go on before it cancels them
    /// (`STEVE_NODE_REMOVE_GRACE_SECS`, default 10 min).
    pub grace: Duration,
    /// On top of the grace period, how long the Hub waits for the Node to be ready: what it
    /// cancelled ending, a link coming back.
    pub settle: Duration,
}

impl Timing {
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Timing {
        let grace = var("STEVE_NODE_REMOVE_GRACE_SECS")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0 && *v <= 24 * 60 * 60)
            .unwrap_or(10 * 60);
        Timing {
            poll: Duration::from_secs(2),
            grace: Duration::from_secs(grace),
            settle: Duration::from_secs(3 * 60),
        }
    }
}

pub struct Removals {
    store: Store,
    fleet: Arc<dyn Fleet>,
    timing: Timing,
    /// Whether each Node said it can remove itself, the last time it was asked.
    seen: Mutex<HashMap<String, bool>>,
}

/// How a removal ended: its phase and why.
type End = (&'static str, Option<String>);

impl Removals {
    pub fn new(store: Store, fleet: Arc<dyn Fleet>, timing: Timing) -> Arc<Self> {
        Arc::new(Removals {
            store,
            fleet,
            timing,
            seen: Mutex::new(HashMap::new()),
        })
    }

    pub fn timing(&self) -> &Timing {
        &self.timing
    }

    pub fn seen(&self, node: &str) -> Option<bool> {
        self.seen.lock().unwrap().get(node).copied()
    }

    /// Asks a linked Node whether it can remove itself (and remembers it); `None`: it is not
    /// linked or did not answer.
    pub async fn look(&self, node: &str) -> Option<bool> {
        let id = NodeId::new(node).ok()?;
        if !self.fleet.linked(node) {
            return None;
        }
        let caps = tokio::time::timeout(
            Duration::from_secs(10),
            self.fleet.discovery(&id).probe(false),
        )
        .await
        .ok()?
        .ok()?;
        let can = caps.features.iter().any(|f| f == FEATURE_NODE_REMOVE);
        self.seen.lock().unwrap().insert(node.into(), can);
        Some(can)
    }

    /// Whether the Hub could ask `node` to remove itself now, and why not: `ok`, `offline`,
    /// `manual` (it cannot: run [`MANUAL_COMMAND`] there), `unknown` (not asked yet), `local` (the
    /// Hub's own) or `unpaired`.
    pub fn assess(&self, node: &NodeRow, linked: bool) -> &'static str {
        if node.local {
            return "local";
        }
        if !matches!(node.state.as_str(), "active" | "paused") {
            return "unpaired";
        }
        if !linked {
            return "offline";
        }
        match self.seen(&node.node_id) {
            None => "unknown",
            Some(false) => "manual",
            Some(true) => "ok",
        }
    }

    /// Starts removing `node` from its machine for its owner `by`; the operation id. Refused with
    /// why (`offline`, `manual`, `busy`) when the Node cannot be asked.
    pub async fn start(self: &Arc<Self>, node: &str, by: &str) -> Result<String, &'static str> {
        if !self.fleet.linked(node) {
            return Err("offline");
        }
        match self.look(node).await {
            None => return Err("offline"),
            Some(false) => return Err("manual"),
            Some(true) => {}
        }
        let op = format!(
            "nrm_{}",
            hex::encode(steve_node_wire::identity::random::<12>().map_err(|_| "internal")?)
        );
        let row = RemovalRow {
            operation_id: op.clone(),
            node_id: node.into(),
            requested_by: by.into(),
            grace_secs: self.timing.grace.as_secs() as i64,
            phase: "requested".into(),
            error: None,
            created: now(),
            updated: now(),
        };
        let (store, node2, by2) = (self.store.clone(), node.to_string(), by.to_string());
        let inserted = tokio::task::spawn_blocking(move || {
            store.transaction(|tx| {
                if !store_nodes::removal_insert(tx, &row)? {
                    return Ok(false);
                }
                store_nodes::audit(tx, Some(&node2), Some(&by2), "remove", None, now())?;
                Ok::<_, steve_store::StoreError>(true)
            })
        })
        .await
        .map_err(|_| "internal")?
        .map_err(|_| "internal")?;
        if !inserted {
            return Err("busy");
        }
        let me = self.clone();
        let op2 = op.clone();
        tokio::spawn(async move { me.run(&op2).await });
        Ok(op)
    }

    async fn row(&self, op: &str) -> Option<RemovalRow> {
        let (store, op) = (self.store.clone(), op.to_string());
        tokio::task::spawn_blocking(move || store.reader(|c| store_nodes::removal_get(c, &op)))
            .await
            .ok()?
            .ok()?
    }

    async fn phase(&self, op: &str, phase: &str, error: Option<String>) {
        let (store, op2, phase2) = (self.store.clone(), op.to_string(), phase.to_string());
        let r = tokio::task::spawn_blocking(move || {
            store.transaction(|tx| {
                store_nodes::removal_phase(tx, &op2, &phase2, error.as_deref(), now())
            })
        })
        .await;
        if !matches!(r, Ok(Ok(_))) {
            tracing::error!("node removal {op}: writing {phase}: {r:?}");
        }
    }

    async fn run(self: Arc<Self>, op: &str) {
        let Some(row) = self.row(op).await else {
            return;
        };
        let node = row.node_id.clone();
        self.fleet.hold(&node, true);
        let (phase, error) = self.drive(&row).await;
        self.finish(&row, phase, error).await;
    }

    /// Records how `row` ended; a removed Node is unpaired, any other takes work again.
    async fn finish(&self, row: &RemovalRow, phase: &'static str, error: Option<String>) {
        let (node, op) = (row.node_id.as_str(), row.operation_id.as_str());
        tracing::info!(
            node,
            op,
            phase,
            error = error.as_deref().unwrap_or(""),
            "node removal"
        );
        if phase == "removed" {
            self.fleet.unpair(node, &row.requested_by).await;
        } else {
            self.fleet.hold(node, false);
        }
        self.phase(op, phase, error).await;
    }

    /// The removal's steps; how it ended.
    async fn drive(&self, row: &RemovalRow) -> End {
        let failed = |e: String| ("failed", Some(e));
        let op = row.operation_id.as_str();
        let (Ok(node), Ok(operation)) = (
            NodeId::new(row.node_id.clone()),
            OperationId::new(op.to_string()),
        ) else {
            return failed("a bad node or operation id".into());
        };
        let d = self.fleet.discovery(&node);
        self.phase(op, "draining", None).await;
        let ask = NodeRemove {
            operation: operation.clone(),
            grace_secs: row.grace_secs.clamp(0, u32::MAX as i64) as u32,
        };
        match d.remove_begin(ask).await {
            Ok(_) => {}
            Err(e) if e.code == ErrorCode::Unsupported => {
                return (
                    "manual",
                    Some("this node cannot remove itself; run the command on its machine".into()),
                );
            }
            // it may have taken it: ask below
            Err(e) if matches!(e.code, ErrorCode::OutcomeUnknown | ErrorCode::Offline) => {}
            Err(e) => return failed(format!("the node refused: {}", e.safe_detail)),
        }
        let limit = Duration::from_secs(row.grace_secs.max(0) as u64) + self.timing.settle;
        let ready = self
            .wait(&*d, op, &operation, limit, |p| {
                p == "ready" || NodeRemoveState::TERMINAL.contains(&p)
            })
            .await;
        match ready {
            Some(s) if s.phase == "ready" => {}
            Some(s) => {
                return failed(
                    s.error
                        .unwrap_or_else(|| format!("the node ended the removal as {}", s.phase)),
                );
            }
            None => {
                let _ = d
                    .remove_commit(RemoveCommit {
                        operation: operation.clone(),
                        abandon: true,
                    })
                    .await;
                return (
                    "unconfirmed",
                    Some(format!(
                        "the node did not get ready within {} s",
                        limit.as_secs()
                    )),
                );
            }
        }
        self.phase(op, "confirming", None).await;
        let commit = RemoveCommit {
            operation: operation.clone(),
            abandon: false,
        };
        match d.remove_commit(commit).await {
            Ok(s) if s.phase == "removing" => ("removed", None),
            Ok(s) => failed(
                s.error
                    .unwrap_or_else(|| format!("the node ended the removal as {}", s.phase)),
            ),
            // the Node drops its link once it answered: ask while it may still be there
            Err(e) if matches!(e.code, ErrorCode::OutcomeUnknown | ErrorCode::Offline) => {
                match self
                    .wait(&*d, op, &operation, Duration::from_secs(30), |p| {
                        NodeRemoveState::TERMINAL.contains(&p)
                    })
                    .await
                {
                    Some(s) if s.phase == "removing" => ("removed", None),
                    _ => (
                        "unconfirmed",
                        Some("the node did not say whether it took the confirmation".into()),
                    ),
                }
            }
            Err(e) => {
                let _ = d
                    .remove_commit(RemoveCommit {
                        operation,
                        abandon: true,
                    })
                    .await;
                failed(format!(
                    "the node refused the confirmation: {}",
                    e.safe_detail
                ))
            }
        }
    }

    /// Asks the Node about `op` until `done` says so or `limit` passes (`None`), keeping the row's
    /// phase with the Node's while it drains. A Node offline meanwhile is asked again.
    async fn wait(
        &self,
        d: &dyn Discovery,
        op: &str,
        operation: &OperationId,
        limit: Duration,
        done: impl Fn(&str) -> bool,
    ) -> Option<NodeRemoveState> {
        let until = tokio::time::Instant::now() + limit;
        let mut last = String::new();
        loop {
            if let Ok(s) = d.remove_status(operation.clone()).await {
                if done(&s.phase) {
                    return Some(s);
                }
                if s.phase != last && matches!(s.phase.as_str(), "draining" | "cancelling") {
                    self.phase(op, &s.phase, None).await;
                    last = s.phase.clone();
                }
            }
            if tokio::time::Instant::now() >= until {
                return None;
            }
            tokio::time::sleep(self.timing.poll).await;
        }
    }

    /// Settles removals a Hub restart left unfinished, from the Node's own state: one the Node
    /// took the confirmation of is unpaired; any other is given up.
    pub async fn settle_open(self: Arc<Self>) {
        let store = self.store.clone();
        let open = tokio::task::spawn_blocking(move || store.reader(store_nodes::removals_open))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        for row in open {
            let me = self.clone();
            tokio::spawn(async move { me.settle(row).await });
        }
    }

    async fn settle(&self, row: RemovalRow) {
        let (Ok(node), Ok(operation)) = (
            NodeId::new(row.node_id.clone()),
            OperationId::new(row.operation_id.clone()),
        ) else {
            return;
        };
        let d = self.fleet.discovery(&node);
        let s = self
            .wait(
                &*d,
                &row.operation_id,
                &operation,
                self.timing.settle,
                |_| true,
            )
            .await;
        let (phase, error): End = match s {
            Some(s) if s.phase == "removing" => ("removed", None),
            Some(s) if !s.is_terminal() => {
                let _ = d
                    .remove_commit(RemoveCommit {
                        operation,
                        abandon: true,
                    })
                    .await;
                (
                    "unconfirmed",
                    Some("the hub restarted during the removal; nothing was removed".into()),
                )
            }
            Some(s) => (
                "failed",
                Some(
                    s.error
                        .unwrap_or_else(|| format!("the node ended the removal as {}", s.phase)),
                ),
            ),
            None => (
                "unconfirmed",
                Some("the hub restarted during the removal; the node never said".into()),
            ),
        };
        self.finish(&row, phase, error).await;
    }

    /// What Devices shows for a Node: whether it can be asked (`reason`, [`Removals::assess`]),
    /// its latest removal, and the command to run on its machine otherwise.
    pub fn view(&self, node: &NodeRow, linked: bool, latest: Option<&RemovalRow>) -> Value {
        json!({
            "reason": self.assess(node, linked),
            "latest": latest,
            "command": MANUAL_COMMAND,
            "grace_secs": self.timing.grace.as_secs(),
        })
    }
}

/// Whether a removal ended.
pub fn done(phase: &str) -> bool {
    REMOVAL_DONE.contains(&phase)
}

// The owner's route (`/api/nodes/{id}/remove`).

use axum::extract::{Path as UrlPath, State};
use axum::response::Response;
use steve_modules::Caller;

use crate::error::json_response;

/// `POST /api/nodes/{id}/remove`: ask the Node to remove itself from its machine (its owner only).
/// `202 {operation}`; `409 {error, command}` when it cannot be asked (`offline`, `manual`,
/// `busy`): `command` is what to run on its machine instead.
pub async fn start(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let Some(rm) = svc.removals.get().cloned() else {
        return json_response(409, &json!({ "error": "off", "command": MANUAL_COMMAND }));
    };
    let (store, me, node) = (svc.store.clone(), caller.me.clone(), id.clone());
    let mine = tokio::task::spawn_blocking(move || {
        store.reader(|c| store_nodes::get(c, &node)).ok().flatten()
    })
    .await
    .ok()
    .flatten()
    .is_some_and(|n| n.owner == me && !n.local && matches!(n.state.as_str(), "active" | "paused"));
    if !mine {
        return json_response(403, &json!({ "error": "denied" }));
    }
    match rm.start(&id, &caller.me).await {
        Ok(op) => json_response(202, &json!({ "operation": op })),
        Err(why) => json_response(409, &json!({ "error": why, "command": MANUAL_COMMAND })),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_manual_command_finds_the_upgraded_release_cli_without_a_path_installation() {
        use std::os::unix::fs::PermissionsExt;
        for stop_code in [0, 7] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().canonicalize().unwrap();
            let cli = home.join(".steve-node/releases/2.0.0/bin/steve");
            let commands = home.join("commands");
            std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
            std::fs::create_dir(&commands).unwrap();
            let log = home.join("calls");
            std::fs::write(&cli, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_LOG\"\n[ \"$2\" = stop ] && exit \"$TEST_STOP_CODE\"\nexit 0\n").unwrap();
            // Never run a real removal or contact the machine's service manager.
            let remove = commands.join("rm");
            std::fs::write(
                &remove,
                "#!/bin/sh\nprintf 'rm %s\\n' \"$*\" >> \"$TEST_LOG\"\n",
            )
            .unwrap();
            for path in [&cli, &remove] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            for (name, program) in [("ls", "/bin/ls"), ("tail", "/usr/bin/tail")] {
                std::os::unix::fs::symlink(program, commands.join(name)).unwrap();
            }
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", super::MANUAL_COMMAND])
                .env_clear()
                .env("HOME", &home)
                .env("PATH", &commands)
                .env("TEST_LOG", &log)
                .env("TEST_STOP_CODE", stop_code.to_string())
                .output()
                .unwrap();
            assert_eq!(
                out.status.success(),
                stop_code == 0,
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let expected = if stop_code == 0 {
                format!(
                    "node stop\nnode uninstall\nrm -rf {}\n",
                    home.join(".steve-node").display()
                )
            } else {
                "node stop\n".into()
            };
            assert_eq!(std::fs::read_to_string(log).unwrap(), expected);
            assert!(cli.is_file());
        }
    }

    use super::*;
    use steve_node_api::testkit::FakeNode;

    struct TestFleet {
        node: Arc<FakeNode>,
        linked: std::sync::atomic::AtomicBool,
        holds: Mutex<Vec<bool>>,
        unpaired: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl Fleet for TestFleet {
        fn discovery(&self, _: &NodeId) -> Arc<dyn Discovery> {
            self.node.clone()
        }
        fn linked(&self, _: &str) -> bool {
            self.linked.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn hold(&self, _: &str, on: bool) {
            self.holds.lock().unwrap().push(on);
        }
        async fn unpair(&self, node: &str, by: &str) -> bool {
            self.unpaired
                .lock()
                .unwrap()
                .push((node.to_string(), by.to_string()));
            true
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        node: Arc<FakeNode>,
        fleet: Arc<TestFleet>,
        rm: Arc<Removals>,
    }

    fn fixture(removable: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .transaction::<_, steve_store::StoreError>(|tx| {
                tx.execute_batch(
                    "INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_a','u','a',zeroblob(32),'fa','active',1);",
                )?;
                Ok(())
            })
            .unwrap();
        let node = FakeNode::new("n_a");
        if removable {
            node.set_features(vec![FEATURE_NODE_REMOVE.into()]);
        }
        let fleet = Arc::new(TestFleet {
            node: node.clone(),
            linked: true.into(),
            holds: Mutex::new(vec![]),
            unpaired: Mutex::new(vec![]),
        });
        let rm = Removals::new(
            store.clone(),
            fleet.clone(),
            Timing {
                poll: Duration::from_millis(10),
                grace: Duration::from_secs(5),
                settle: Duration::from_millis(200),
            },
        );
        Fixture {
            _dir: dir,
            store,
            node,
            fleet,
            rm,
        }
    }

    async fn ended(f: &Fixture, op: &str) -> RemovalRow {
        for _ in 0..1000 {
            let r = f.rm.row(op).await.unwrap();
            if done(&r.phase) {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the removal did not end");
    }

    #[tokio::test]
    async fn the_node_is_asked_confirmed_and_unpaired_once_it_took_the_confirmation() {
        let f = fixture(true);
        let op = f.rm.start("n_a", "u").await.unwrap();
        let r = ended(&f, &op).await;
        assert_eq!(r.phase, "removed", "{:?}", r.error);
        assert_eq!(r.grace_secs, 5, "the grace period it was asked with");
        let id = OperationId::new(op.clone()).unwrap();
        assert_eq!(
            f.node.removal_calls(),
            vec![("begin", id.clone()), ("commit", id)],
            "begun, then confirmed once it was ready"
        );
        assert_eq!(
            *f.fleet.unpaired.lock().unwrap(),
            vec![("n_a".to_string(), "u".to_string())]
        );
        // held from the start, never let go: it is unpaired
        assert_eq!(*f.fleet.holds.lock().unwrap(), vec![true]);
        let audit =
            steve_store::testkit::hub::node_audit_count(&f.store, "n_a", "u", "remove").unwrap();
        assert_eq!(audit, 1);
    }

    #[tokio::test]
    async fn an_offline_node_or_one_of_an_earlier_release_is_shown_the_command_instead() {
        let offline = fixture(true);
        offline
            .fleet
            .linked
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(offline.rm.start("n_a", "u").await.unwrap_err(), "offline");
        let old = fixture(false);
        assert_eq!(old.rm.start("n_a", "u").await.unwrap_err(), "manual");
        for f in [&offline, &old] {
            assert!(f.node.removal_calls().is_empty(), "nothing sent");
            assert!(f.fleet.unpaired.lock().unwrap().is_empty(), "still paired");
            let n = f
                .store
                .reader(|c| store_nodes::get(c, "n_a"))
                .unwrap()
                .unwrap();
            let v = f.rm.view(&n, f.fleet.linked("n_a"), None);
            assert_eq!(v["command"], MANUAL_COMMAND);
        }
        let n = old
            .store
            .reader(|c| store_nodes::get(c, "n_a"))
            .unwrap()
            .unwrap();
        assert_eq!(old.rm.view(&n, true, None)["reason"], "manual");
        assert_eq!(offline.rm.view(&n, false, None)["reason"], "offline");
    }

    #[tokio::test]
    async fn a_node_that_never_gets_ready_is_not_unpaired_and_gives_it_up() {
        let f = fixture(true);
        f.node.set_remove_stall(true);
        let op = f.rm.start("n_a", "u").await.unwrap();
        let r = ended(&f, &op).await;
        assert_eq!(r.phase, "unconfirmed");
        assert!(r.error.unwrap().contains("did not get ready"));
        let id = OperationId::new(op).unwrap();
        assert_eq!(
            f.node.removal_calls(),
            vec![("begin", id.clone()), ("abandon", id)],
            "never confirmed; given up on the node too"
        );
        assert!(f.fleet.unpaired.lock().unwrap().is_empty());
        assert_eq!(
            *f.fleet.holds.lock().unwrap(),
            vec![true, false],
            "takes work again"
        );
    }

    #[tokio::test]
    async fn one_removal_at_a_time_and_a_hub_restart_settles_from_the_nodes_state() {
        let f = fixture(true);
        f.node.set_remove_stall(true);
        let op = f.rm.start("n_a", "u").await.unwrap();
        assert_eq!(f.rm.start("n_a", "u").await.unwrap_err(), "busy");
        ended(&f, &op).await;

        // a removal the node took the confirmation of before the hub restarted: unpaired
        let f = fixture(true);
        let id = OperationId::new("nrm_x").unwrap();
        f.node
            .remove_begin(NodeRemove {
                operation: id.clone(),
                grace_secs: 5,
            })
            .await
            .unwrap();
        f.node
            .remove_commit(RemoveCommit {
                operation: id,
                abandon: false,
            })
            .await
            .unwrap();
        f.store
            .transaction(|tx| {
                store_nodes::removal_insert(
                    tx,
                    &RemovalRow {
                        operation_id: "nrm_x".into(),
                        node_id: "n_a".into(),
                        requested_by: "u".into(),
                        grace_secs: 5,
                        phase: "confirming".into(),
                        error: None,
                        created: 1,
                        updated: 1,
                    },
                )
            })
            .unwrap();
        f.rm.clone().settle_open().await;
        let r = ended(&f, "nrm_x").await;
        assert_eq!(r.phase, "removed");
        assert_eq!(f.fleet.unpaired.lock().unwrap().len(), 1);
    }
}
