//! Upgrading paired Nodes to the Hub's own release (the Hub decides, the Node executes).
//!
//! A Node can be upgraded when its probe says [`FEATURE_NODE_UPGRADE`] and its build: never
//! guessed from a version number. Its owner's policy (`nodes.upgrade_policy`, else the Hub's
//! default, `notify` unless set) says whether the Hub does it on its own (`auto`), only shows it
//! (`notify`) or neither (`off`); the instance owner may always ask from Devices.
//!
//! One upgrade, `node_upgrades` row by row (each phase written before it is taken):
//!
//! 1. `preparing`: the archive for the Node's target — a bundle of this Hub's own `steve-node` when
//!    the Node is the Hub's platform, else the published installer archive checked against the
//!    release's manifest ([`super::install::installer`]) — kept under `<data>/node-upgrades`.
//! 2. `staging` → `staged`: the Node fetches it over its link (`blob.fetch` under the upgrade's
//!    operation: only that Node, only that file, only while it stages), checks it and unpacks it
//!    beside its release ([`Discovery::upgrade_stage`]).
//! 3. `draining`: no new turns go to the Node; the Hub waits for its running ones to end. Past the
//!    drain time the upgrade is given up, unless it was forced (running turns then end with the
//!    Node's restart and are continued as after any restart).
//! 4. `switching`: the Node switches and goes back on its own if the new release does not link in
//!    time ([`Discovery::upgrade_switch`]); the Hub reads where it ended up.
//!
//! A lost answer is asked about ([`Discovery::upgrade_status`]), never sent again. After a Hub
//! restart an unfinished upgrade is settled from the Node's state, never run on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_node_api::*;
use steve_store::Store;
use steve_store::nodes::{self as store_nodes, UPGRADE_DONE, UpgradeRow};

use super::now;

/// The artifact ids of upgrade archives (`node-upgrade.<operation>`).
pub const ARTIFACT_PREFIX: &str = "node-upgrade.";

/// What the orchestration needs of the fleet, so tests can stand in.
pub trait Fleet: Send + Sync {
    fn discovery(&self, node: &NodeId) -> Arc<dyn Discovery>;
    fn linked(&self, node: &str) -> bool;
    /// Stop (or start again) giving the Node new turns.
    fn hold(&self, node: &str, on: bool);
}

/// The real fleet: each Node's link and coordinator.
pub struct LiveFleet {
    pub fleet: Arc<super::NodeFleet>,
    pub registry: Arc<super::NodeRegistry>,
}

impl Fleet for LiveFleet {
    fn discovery(&self, node: &NodeId) -> Arc<dyn Discovery> {
        self.fleet.remote(node)
    }
    fn linked(&self, node: &str) -> bool {
        self.registry.current(node).is_some()
    }
    fn hold(&self, node: &str, on: bool) {
        if let Some(rt) = self.fleet.runtime_for(node) {
            rt.set_paused(on);
        }
    }
}

#[derive(Debug, Clone)]
pub struct Timing {
    pub poll: Duration,
    /// Fetching and unpacking on the Node.
    pub stage: Duration,
    /// Waiting for the Node's turns to end (`STEVE_NODE_UPGRADE_DRAIN_SECS`, default 30 min).
    pub drain: Duration,
    /// The new release's time to link before the Node goes back (`STEVE_NODE_UPGRADE_HEALTH_SECS`,
    /// default 180 s).
    pub health: Duration,
}

impl Timing {
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Timing {
        let secs = |k: &str, d: u64| {
            var(k)
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(d)
        };
        Timing {
            poll: Duration::from_secs(2),
            stage: Duration::from_secs(30 * 60),
            drain: Duration::from_secs(secs("STEVE_NODE_UPGRADE_DRAIN_SECS", 30 * 60)),
            health: Duration::from_secs(secs("STEVE_NODE_UPGRADE_HEALTH_SECS", 180)),
        }
    }
}

/// What a Node told the Hub about itself the last time it was asked.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub build: Option<BuildIdentity>,
    pub upgradable: bool,
    pub at: i64,
}

pub struct Upgrades {
    store: Store,
    data_dir: PathBuf,
    fleet: Arc<dyn Fleet>,
    /// The Hub's own build; `None` (a development build): nothing to upgrade to.
    hub: Option<BuildIdentity>,
    /// The Hub's release directory (`release.json`, `bin/steve-node`), for Nodes of its platform.
    release_dir: Option<PathBuf>,
    timing: Timing,
    seen: Mutex<HashMap<String, Seen>>,
}

fn hub_build() -> Option<BuildIdentity> {
    Some(BuildIdentity {
        version: option_env!("STEVE_BUILD_VERSION")?.into(),
        commit: option_env!("STEVE_BUILD_COMMIT")?.into(),
        target: env!("STEVE_BUILD_TARGET").into(),
    })
}

/// `<release>/bin/steve-hub` → `<release>`, when it holds a release manifest.
fn own_release_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let dir = exe.parent()?.parent()?.to_path_buf();
    dir.join("release.json").is_file().then_some(dir)
}

impl Upgrades {
    pub fn new(
        store: Store,
        data_dir: PathBuf,
        fleet: Arc<dyn Fleet>,
        timing: Timing,
    ) -> Arc<Self> {
        Self::with(
            store,
            data_dir,
            fleet,
            timing,
            hub_build(),
            own_release_dir(),
        )
    }

    pub fn with(
        store: Store,
        data_dir: PathBuf,
        fleet: Arc<dyn Fleet>,
        timing: Timing,
        hub: Option<BuildIdentity>,
        release_dir: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Upgrades {
            store,
            data_dir,
            fleet,
            hub,
            release_dir,
            timing,
            seen: Mutex::new(HashMap::new()),
        })
    }

    pub fn hub(&self) -> Option<&BuildIdentity> {
        self.hub.as_ref()
    }

    pub fn seen(&self, node: &str) -> Option<Seen> {
        self.seen.lock().unwrap().get(node).cloned()
    }

    /// Asks a linked Node what it runs (and remembers it).
    pub async fn look(&self, node: &str) -> Option<Seen> {
        let id = NodeId::new(node).ok()?;
        if !self.fleet.linked(node) {
            return self.seen(node);
        }
        let caps = tokio::time::timeout(
            Duration::from_secs(10),
            self.fleet.discovery(&id).probe(false),
        )
        .await
        .ok()?
        .ok()?;
        let s = Seen {
            upgradable: caps.features.iter().any(|f| f == FEATURE_NODE_UPGRADE),
            build: caps.build,
            at: now(),
        };
        self.seen.lock().unwrap().insert(node.into(), s.clone());
        Some(s)
    }

    /// Whether the Hub could upgrade `node` now, and why not.
    pub fn assess(&self, seen: Option<&Seen>) -> (bool, &'static str) {
        let Some(hub) = &self.hub else {
            return (false, "hub_dev_build");
        };
        let Some(seen) = seen else {
            return (false, "unknown");
        };
        if !seen.upgradable {
            return (false, "manual");
        }
        match &seen.build {
            None => (false, "unknown"),
            Some(b) if b.version == hub.version && b.commit == hub.commit => (false, "current"),
            Some(_) => (true, "outdated"),
        }
    }

    /// Starts upgrading `node` to the Hub's release for `by`; the operation id. Refused (with why)
    /// for a Node that cannot be or is being upgraded.
    pub async fn start(
        self: &Arc<Self>,
        node: &str,
        by: &str,
        force: bool,
    ) -> Result<String, String> {
        let hub = self.hub.clone().ok_or("this hub is a development build")?;
        let seen = self.look(node).await;
        let (ok, why) = self.assess(seen.as_ref());
        if !ok {
            return Err(why.into());
        }
        let build = seen.and_then(|s| s.build).ok_or("unknown")?;
        let op = format!(
            "nup_{}",
            hex::encode(steve_node_wire::identity::random::<12>().map_err(|e| e.to_string())?)
        );
        let row = UpgradeRow {
            operation_id: op.clone(),
            node_id: node.into(),
            version: hub.version.clone(),
            commit_sha: hub.commit.clone(),
            target: build.target.clone(),
            from_version: Some(build.version.clone()),
            artifact: format!("{ARTIFACT_PREFIX}{op}"),
            file: String::new(),
            sha256: String::new(),
            size: 0,
            requested_by: by.into(),
            force,
            phase: "preparing".into(),
            error: None,
            created: now(),
            updated: now(),
        };
        let store = self.store.clone();
        let inserted = tokio::task::spawn_blocking(move || {
            store.transaction(|tx| store_nodes::upgrade_insert(tx, &row))
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        if !inserted {
            return Err("busy".into());
        }
        let me = self.clone();
        let op2 = op.clone();
        tokio::spawn(async move { me.run(&op2).await });
        Ok(op)
    }

    async fn row(&self, op: &str) -> Option<UpgradeRow> {
        let (store, op) = (self.store.clone(), op.to_string());
        tokio::task::spawn_blocking(move || store.reader(|c| store_nodes::upgrade_get(c, &op)))
            .await
            .ok()?
            .ok()?
    }

    async fn phase(&self, op: &str, phase: &str, error: Option<String>) {
        let (store, op2, phase2) = (self.store.clone(), op.to_string(), phase.to_string());
        let r = tokio::task::spawn_blocking(move || {
            store.transaction(|tx| {
                store_nodes::upgrade_phase(tx, &op2, &phase2, error.as_deref(), now())
            })
        })
        .await;
        if !matches!(r, Ok(Ok(_))) {
            tracing::error!("node upgrade {op}: writing {phase}: {r:?}");
        }
    }

    async fn running(&self, node: &str) -> i64 {
        let (store, node) = (self.store.clone(), node.to_string());
        tokio::task::spawn_blocking(move || {
            store.reader(|c| steve_store::runtime::session_room(c, &node))
        })
        .await
        .ok()
        .and_then(Result::ok)
        .map_or(0, |r| r.running)
    }

    async fn run(self: Arc<Self>, op: &str) {
        let Some(row) = self.row(op).await else {
            return;
        };
        let node = row.node_id.clone();
        let mut held = false;
        let end = self.drive(&row, &mut held).await;
        if held {
            self.fleet.hold(&node, false);
        }
        let (phase, error) = match end {
            Ok(phase) => (phase, None),
            Err((phase, e)) => (phase, Some(e)),
        };
        tracing::info!(
            node,
            op,
            phase,
            error = error.as_deref().unwrap_or(""),
            "node upgrade"
        );
        self.phase(op, &phase, error).await;
        self.look(&node).await;
    }

    /// The upgrade's steps; how it ended.
    async fn drive(&self, row: &UpgradeRow, held: &mut bool) -> Result<String, (String, String)> {
        let failed = |e: String| ("failed".to_string(), e);
        let op = row.operation_id.as_str();
        let node = NodeId::new(row.node_id.clone()).map_err(|e| failed(e.to_string()))?;
        let operation = OperationId::new(op).map_err(|e| failed(e.to_string()))?;
        let build = BuildIdentity {
            version: row.version.clone(),
            commit: row.commit_sha.clone(),
            target: row.target.clone(),
        };
        let (file, sha256, size) = self
            .artifact(&build)
            .await
            .map_err(|e| failed(format!("preparing the release: {e:#}")))?;
        {
            let (store, op2, file2, sha2) = (
                self.store.clone(),
                op.to_string(),
                file.to_string_lossy().into_owned(),
                sha256.clone(),
            );
            tokio::task::spawn_blocking(move || {
                store.transaction(|tx| {
                    tx.execute(
                        "UPDATE node_upgrades SET file=?2, sha256=?3, size=?4 WHERE operation_id=?1",
                        rusqlite::params![op2, file2, sha2, size as i64],
                    )
                    .map_err(steve_store::StoreError::from)
                })
            })
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(|e| failed(e.to_string()))?;
        }
        self.phase(op, "staging", None).await;
        let d = self.fleet.discovery(&node);
        let ask = NodeUpgrade {
            operation: operation.clone(),
            build: build.clone(),
            artifact: ArtifactId::new(row.artifact.clone()).map_err(|e| failed(e.to_string()))?,
            size,
            sha256,
            health_secs: self.timing.health.as_secs() as u32,
        };
        match d.upgrade_stage(ask).await {
            Ok(_) => {}
            // it may have taken it: ask below
            Err(e) if matches!(e.code, ErrorCode::OutcomeUnknown | ErrorCode::Offline) => {}
            Err(e) => return Err(failed(format!("the node refused: {}", e.safe_detail))),
        }
        let s = self
            .wait(&*d, &operation, self.timing.stage, |p| p != "staging")
            .await
            .ok_or_else(|| failed("the node did not finish staging in time".into()))?;
        if s.phase != "staged" {
            return Err((s.phase.clone(), s.error.unwrap_or_default()));
        }
        self.phase(op, "draining", None).await;
        self.fleet.hold(&row.node_id, true);
        *held = true;
        let until = tokio::time::Instant::now() + self.timing.drain;
        let mut force = false;
        loop {
            let running = self.running(&row.node_id).await;
            if running == 0 {
                break;
            }
            if tokio::time::Instant::now() >= until {
                if row.force {
                    force = true;
                    break;
                }
                let _ = d
                    .upgrade_switch(UpgradeSwitch {
                        operation: operation.clone(),
                        abandon: true,
                        force: false,
                    })
                    .await;
                return Err(failed(format!(
                    "{running} turns still ran after {} s; nothing was switched",
                    self.timing.drain.as_secs()
                )));
            }
            tokio::time::sleep(self.timing.poll).await;
        }
        self.phase(op, "switching", None).await;
        // the Node may still finish something the Hub counted as done: a little patience
        loop {
            match d
                .upgrade_switch(UpgradeSwitch {
                    operation: operation.clone(),
                    abandon: false,
                    force,
                })
                .await
            {
                Ok(_) => break,
                Err(e) if e.code == ErrorCode::Busy && tokio::time::Instant::now() < until => {
                    tokio::time::sleep(self.timing.poll).await;
                }
                Err(e) if matches!(e.code, ErrorCode::OutcomeUnknown | ErrorCode::Offline) => {
                    break;
                }
                Err(e) => {
                    let _ = d
                        .upgrade_switch(UpgradeSwitch {
                            operation: operation.clone(),
                            abandon: true,
                            force: false,
                        })
                        .await;
                    return Err(failed(format!(
                        "the node refused to switch: {}",
                        e.safe_detail
                    )));
                }
            }
        }
        let wait = self.timing.health + Duration::from_secs(5 * 60);
        match self
            .wait(&*d, &operation, wait, |p| {
                UPGRADE_DONE.contains(&p) || p == "abandoned"
            })
            .await
        {
            Some(s) if s.phase == "succeeded" => Ok("succeeded".into()),
            Some(s) => Err((s.phase, s.error.unwrap_or_default())),
            None => Err((
                "unknown".into(),
                "the node did not say how the switch ended".into(),
            )),
        }
    }

    /// Asks the Node about `op` until `done` says so or `limit` passes (`None`). A Node offline
    /// meanwhile (restarting) is asked again.
    async fn wait(
        &self,
        d: &dyn Discovery,
        op: &OperationId,
        limit: Duration,
        done: impl Fn(&str) -> bool,
    ) -> Option<NodeUpgradeState> {
        let until = tokio::time::Instant::now() + limit;
        loop {
            if let Ok(s) = d.upgrade_status(op.clone()).await
                && done(&s.phase)
            {
                return Some(s);
            }
            if tokio::time::Instant::now() >= until {
                return None;
            }
            tokio::time::sleep(self.timing.poll).await;
        }
    }

    /// The archive for `build` (the Hub's), as a file under `<data>/node-upgrades`: its path,
    /// SHA256 and size.
    async fn artifact(&self, build: &BuildIdentity) -> anyhow::Result<(PathBuf, String, u64)> {
        let dir = self.data_dir.join("node-upgrades");
        tokio::fs::create_dir_all(&dir).await?;
        let file = dir.join(format!(
            "steve-node-{}-{}.tar.gz",
            build.version, build.target
        ));
        let hub = self.hub.clone().filter(|h| h.target == build.target);
        let bytes = match (hub, &self.release_dir) {
            (Some(_), Some(release)) => {
                let (release, b) = (release.clone(), build.clone());
                tokio::task::spawn_blocking(move || own_bundle(&release, &b)).await??
            }
            _ => super::install::installer(&build.version, &build.target).await?,
        };
        let sha = hex::encode(Sha256::digest(&bytes));
        let tmp = file.with_extension("tmp");
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, &file).await?;
        Ok((file, sha, bytes.len() as u64))
    }

    /// Settles upgrades a Hub restart left unfinished, from the Node's own state.
    pub async fn settle_open(self: &Arc<Self>) {
        let store = self.store.clone();
        let open = tokio::task::spawn_blocking(move || store.reader(store_nodes::upgrades_open))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        for row in open {
            let me = self.clone();
            tokio::spawn(async move { me.settle(row).await });
        }
    }

    async fn settle(&self, row: UpgradeRow) {
        let op = row.operation_id.clone();
        let (Ok(node), Ok(operation)) = (
            NodeId::new(row.node_id.clone()),
            OperationId::new(op.clone()),
        ) else {
            return;
        };
        let d = self.fleet.discovery(&node);
        let limit = self.timing.health + Duration::from_secs(10 * 60);
        let s = self.wait(&*d, &operation, limit, |_| true).await;
        let (phase, error) = match s {
            None => (
                "unknown".to_string(),
                Some("the hub restarted; the node never said".to_string()),
            ),
            Some(s) if s.phase == "staged" || s.phase == "staging" => {
                let _ = d
                    .upgrade_switch(UpgradeSwitch {
                        operation: operation.clone(),
                        abandon: true,
                        force: false,
                    })
                    .await;
                (
                    "failed".into(),
                    Some("the hub restarted during the upgrade".into()),
                )
            }
            Some(s) if s.is_terminal() => (s.phase.clone(), s.error.clone()),
            // switching or linked: it ends on its own
            Some(_) => match self
                .wait(&*d, &operation, limit, |p| {
                    NodeUpgradeState::TERMINAL.contains(&p)
                })
                .await
            {
                Some(s) => (s.phase.clone(), s.error.clone()),
                None => (
                    "unknown".into(),
                    Some("the node did not say how the switch ended".into()),
                ),
            },
        };
        self.phase(&op, &phase, error).await;
    }

    /// One pass of the `auto` policy: every linked Node that may be upgraded on its own and runs
    /// another build than the Hub, unless an upgrade to this very build already failed there (its
    /// owner can still ask).
    pub async fn sweep(self: &Arc<Self>) {
        let Some(hub) = self.hub.clone() else { return };
        let store = self.store.clone();
        let rows = tokio::task::spawn_blocking(move || {
            store.reader(|c| {
                let default = store_nodes::upgrade_default(c)?;
                store_nodes::list_live(c)?
                    .into_iter()
                    .filter(|n| !n.local && n.state == "active")
                    .map(|n| {
                        let latest = store_nodes::upgrade_latest(c, &n.node_id)?;
                        let policy = n.upgrade_policy.clone().unwrap_or_else(|| default.clone());
                        Ok((n, policy, latest))
                    })
                    .collect::<steve_store::Result<Vec<_>>>()
            })
        })
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
        for (n, policy, latest) in rows {
            if !self.fleet.linked(&n.node_id) {
                continue;
            }
            let seen = self.look(&n.node_id).await;
            if policy != "auto" || !self.assess(seen.as_ref()).0 {
                continue;
            }
            if latest.as_ref().is_some_and(|l| {
                !UPGRADE_DONE.contains(&l.phase.as_str()) || l.version == hub.version
            }) {
                continue;
            }
            match self.start(&n.node_id, "hub", false).await {
                Ok(op) => tracing::info!(
                    node = n.node_id,
                    op,
                    "upgrading the node to the hub's release"
                ),
                Err(e) => tracing::debug!(node = n.node_id, "node upgrade not started: {e}"),
            }
        }
    }

    /// Keeps Nodes on the Hub's release while the Hub runs (`auto` policy), first settling what a
    /// restart left.
    pub async fn follow(self: Arc<Self>) {
        self.settle_open().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            self.sweep().await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    /// What Devices shows for a Node.
    pub fn view(
        &self,
        node: &steve_store::nodes::NodeRow,
        default: &str,
        latest: Option<&UpgradeRow>,
    ) -> Value {
        let seen = self.seen(&node.node_id);
        let (can, why) = self.assess(seen.as_ref());
        json!({
            "build": seen.as_ref().and_then(|s| s.build.clone()),
            "policy": node.upgrade_policy.clone().unwrap_or_else(|| default.to_string()),
            "own_policy": node.upgrade_policy,
            "can": can,
            "reason": why,
            "latest": latest,
        })
    }
}

/// A Node bundle of the Hub's own release: its manifest and its `steve-node`.
fn own_bundle(release: &Path, build: &BuildIdentity) -> anyhow::Result<Vec<u8>> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(release.join("release.json"))?)?;
    anyhow::ensure!(
        manifest["version"] == build.version.as_str()
            && manifest["commit"] == build.commit.as_str(),
        "the hub's release directory is not the running release"
    );
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    let mut add = |name: &str, data: &[u8], mode: u32| -> std::io::Result<()> {
        let mut h = tar::Header::new_ustar();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mtime(0);
        tar.append_data(&mut h, name, data)
    };
    add(
        "release.json",
        &std::fs::read(release.join("release.json"))?,
        0o644,
    )?;
    add(
        "bin/steve-node",
        &std::fs::read(release.join("bin/steve-node"))?,
        0o755,
    )?;
    Ok(tar.into_inner()?.finish()?)
}

/// `blob.fetch` of an upgrade archive: granted to the Node the upgrade is for, under its
/// operation, while it stages.
pub fn read_chunk(
    store: &Store,
    data_dir: &Path,
    node: &NodeId,
    operation: &OperationId,
    id: &ArtifactId,
    offset: u64,
    max: u32,
) -> ApiResult<ReadChunk> {
    use std::io::{Read, Seek, SeekFrom};
    let denied = || ApiError::new(ErrorCode::Denied, "artifact not granted to this operation");
    let row = store
        .reader(|c| store_nodes::upgrade_get(c, operation.as_str()))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
        .ok_or_else(denied)?;
    let path = PathBuf::from(&row.file);
    if row.node_id != node.as_str()
        || row.artifact != id.as_str()
        || row.phase != "staging"
        || row.size <= 0
        || !path.starts_with(data_dir.join("node-upgrades"))
    {
        return Err(denied());
    }
    if max == 0 || offset > row.size as u64 {
        return Err(ApiError::new(ErrorCode::Invalid, "bad read"));
    }
    let mut f = std::fs::File::open(&path)
        .map_err(|_| ApiError::new(ErrorCode::Conflict, "the upgrade archive is gone"))?;
    let size = f
        .metadata()
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
        .len();
    if size != row.size as u64 {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "the upgrade archive changed",
        ));
    }
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    let len = (max.min(256 * 1024) as u64).min(size - offset) as usize;
    let mut bytes = vec![0; len];
    f.read_exact(&mut bytes)
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(ReadChunk {
        offset,
        eof: offset + len as u64 == size,
        bytes,
    })
}

// The owner's routes (`/api/nodes/...`).

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::response::Response;
use steve_modules::Caller;

use super::NodeService;
use crate::error::json_response;

fn err(status: u16, e: &str) -> Response {
    json_response(status, &json!({ "error": e }))
}

/// Upgrading Nodes is the instance owner's.
fn instance_owner(svc: &NodeService, me: &str) -> bool {
    svc.store
        .access()
        .owner()
        .ok()
        .flatten()
        .is_some_and(|u| u.id == me)
}

#[derive(serde::Deserialize, Default)]
pub struct StartBody {
    #[serde(default)]
    force: bool,
}

/// `POST /api/nodes/{id}/upgrade` `{force?}`: upgrade the Node to the Hub's release now.
pub async fn start(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
    body: Option<Json<StartBody>>,
) -> Response {
    let Some(up) = svc.upgrades.get().cloned() else {
        return err(409, "off");
    };
    let (store, me, node) = (svc.store.clone(), caller.me.clone(), id.clone());
    let mine = tokio::task::spawn_blocking(move || {
        store.reader(|c| store_nodes::get(c, &node)).ok().flatten()
    })
    .await
    .ok()
    .flatten()
    .is_some_and(|n| n.owner == me && !n.local && n.state == "active");
    if !mine || !instance_owner(&svc, &caller.me) {
        return err(403, "denied");
    }
    let force = body.map(|b| b.0.force).unwrap_or(false);
    match up.start(&id, &caller.me, force).await {
        Ok(op) => json_response(202, &json!({ "operation": op })),
        Err(why) => err(409, &why),
    }
}

/// `GET /api/nodes/upgrade-default`, `PUT` `{policy}`: the Hub's default for Nodes without their
/// own policy (`auto` | `notify` | `off`).
pub async fn get_default(State(svc): State<Arc<NodeService>>, _caller: Caller) -> Response {
    let store = svc.store.clone();
    match tokio::task::spawn_blocking(move || store.reader(store_nodes::upgrade_default)).await {
        Ok(Ok(p)) => json_response(
            200,
            &json!({ "policy": p, "hub": svc.upgrades.get().and_then(|u| u.hub().cloned()) }),
        ),
        _ => err(500, "internal"),
    }
}

#[derive(serde::Deserialize)]
pub struct PolicyBody {
    policy: String,
}

pub async fn put_default(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    Json(b): Json<PolicyBody>,
) -> Response {
    if !instance_owner(&svc, &caller.me) {
        return err(403, "denied");
    }
    let store = svc.store.clone();
    let p = b.policy.clone();
    match tokio::task::spawn_blocking(move || {
        store.transaction(|tx| store_nodes::set_upgrade_default(tx, &p, now()))
    })
    .await
    {
        Ok(Ok(true)) => json_response(200, &json!({ "policy": b.policy })),
        Ok(Ok(false)) => err(400, "invalid policy"),
        _ => err(500, "internal"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_node_api::testkit::FakeNode;

    const TARGET: &str = env!("STEVE_BUILD_TARGET");

    fn build(v: &str, c: char) -> BuildIdentity {
        BuildIdentity {
            version: v.into(),
            commit: c.to_string().repeat(40),
            target: TARGET.into(),
        }
    }

    struct TestFleet {
        node: Arc<FakeNode>,
        holds: Mutex<Vec<bool>>,
    }

    impl Fleet for TestFleet {
        fn discovery(&self, _: &NodeId) -> Arc<dyn Discovery> {
            self.node.clone()
        }
        fn linked(&self, _: &str) -> bool {
            true
        }
        fn hold(&self, _: &str, on: bool) {
            self.holds.lock().unwrap().push(on);
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        data: PathBuf,
        node: Arc<FakeNode>,
        fleet: Arc<TestFleet>,
        up: Arc<Upgrades>,
    }

    fn fixture(features: bool, hub: Option<BuildIdentity>, drain_ms: u64) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .transaction::<_, steve_store::StoreError>(|tx| {
                tx.execute_batch(
                    "INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_a','u','a',zeroblob(32),'fa','active',1);",
                )?;
                Ok(())
            })
            .unwrap();
        let release = dir.path().join("release");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        let h = hub.clone().unwrap_or_else(|| build("0.0.0", 'f'));
        std::fs::write(
            release.join("release.json"),
            json!({"format_version":1,"version":h.version,"commit":h.commit}).to_string(),
        )
        .unwrap();
        std::fs::write(release.join("bin/steve-node"), b"#!/bin/sh\n").unwrap();
        let node = FakeNode::new("n_a");
        if features {
            node.set_features(vec![FEATURE_NODE_UPGRADE.into()]);
        }
        node.set_build(Some(build("1.0.0", 'a')));
        let fleet = Arc::new(TestFleet {
            node: node.clone(),
            holds: Mutex::new(vec![]),
        });
        let timing = Timing {
            poll: Duration::from_millis(20),
            stage: Duration::from_secs(5),
            drain: Duration::from_millis(drain_ms),
            health: Duration::from_secs(1),
        };
        let data = dir.path().join("data");
        let up = Upgrades::with(
            store.clone(),
            data.clone(),
            fleet.clone(),
            timing,
            hub,
            Some(release),
        );
        Fixture {
            _dir: dir,
            store,
            data,
            node,
            fleet,
            up,
        }
    }

    async fn ended(f: &Fixture, op: &str) -> UpgradeRow {
        for _ in 0..500 {
            let r = f.up.row(op).await.unwrap();
            if UPGRADE_DONE.contains(&r.phase.as_str()) {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the upgrade did not end");
    }

    #[tokio::test]
    async fn an_outdated_node_is_staged_drained_and_switched_to_the_hubs_own_build() {
        let f = fixture(true, Some(build("2.0.0", 'b')), 1000);
        let op = f.up.start("n_a", "u", false).await.unwrap();
        let r = ended(&f, &op).await;
        assert_eq!(r.phase, "succeeded", "{:?}", r.error);
        assert_eq!(r.from_version.as_deref(), Some("1.0.0"));
        // what the node was asked to stage is the archive the hub kept, checksum and all
        let staged = f.node.staged_upgrades();
        assert_eq!(staged.len(), 1);
        let bytes = std::fs::read(&r.file).unwrap();
        assert_eq!(staged[0].sha256, hex::encode(Sha256::digest(&bytes)));
        assert_eq!(staged[0].size, bytes.len() as u64);
        assert_eq!(staged[0].build, build("2.0.0", 'b'));
        // the hub's own steve-node, in a bundle the node's checks read
        let mut names: Vec<String> = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]))
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["bin/steve-node", "release.json"]);
        // new turns held only while it switched
        assert_eq!(*f.fleet.holds.lock().unwrap(), vec![true, false]);
        assert_eq!(f.up.seen("n_a").unwrap().build, Some(build("2.0.0", 'b')));
        // done: now current, nothing more to do
        assert_eq!(f.up.start("n_a", "u", false).await.unwrap_err(), "current");
    }

    #[tokio::test]
    async fn a_switch_the_node_rolled_back_is_reported_as_such() {
        let f = fixture(true, Some(build("2.0.0", 'b')), 1000);
        f.node
            .set_upgrade_outcome("rolled_back", Some("the new release did not link"));
        let op = f.up.start("n_a", "u", false).await.unwrap();
        let r = ended(&f, &op).await;
        assert_eq!(r.phase, "rolled_back");
        assert!(r.error.unwrap().contains("did not link"));
        assert_eq!(f.up.seen("n_a").unwrap().build, Some(build("1.0.0", 'a')));
    }

    #[tokio::test]
    async fn running_turns_past_the_drain_time_give_it_up_unless_forced() {
        for force in [false, true] {
            let f = fixture(true, Some(build("2.0.0", 'b')), 100);
            f.store
                .transaction::<_, steve_store::StoreError>(|tx| {
                    tx.execute_batch(
                        "INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,created,updated) VALUES ('op_t','n_a','start_turn','{}','d','active',1,1);",
                    )?;
                    Ok(())
                })
                .unwrap();
            let op = f.up.start("n_a", "u", force).await.unwrap();
            let r = ended(&f, &op).await;
            if force {
                assert_eq!(r.phase, "succeeded");
            } else {
                assert_eq!(r.phase, "failed");
                assert!(r.error.unwrap().contains("still ran"));
                let s = f
                    .node
                    .upgrade_status(OperationId::new(op.clone()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(s.phase, "abandoned", "the staged release is given up");
            }
            assert_eq!(f.fleet.holds.lock().unwrap().last(), Some(&false));
        }
    }

    #[tokio::test]
    async fn only_a_node_that_says_so_and_runs_another_build_is_upgraded() {
        let manual = fixture(false, Some(build("2.0.0", 'b')), 1000);
        assert_eq!(
            manual.up.start("n_a", "u", false).await.unwrap_err(),
            "manual"
        );
        let dev = fixture(true, None, 1000);
        assert!(dev.up.start("n_a", "u", false).await.is_err());
        let current = fixture(true, Some(build("1.0.0", 'a')), 1000);
        assert_eq!(
            current.up.start("n_a", "u", false).await.unwrap_err(),
            "current"
        );
        // same version, another commit: still another build
        let rebuilt = fixture(true, Some(build("1.0.0", 'c')), 1000);
        assert!(rebuilt.up.start("n_a", "u", false).await.is_ok());
    }

    #[tokio::test]
    async fn the_auto_policy_upgrades_once_and_does_not_retry_a_failure() {
        let f = fixture(true, Some(build("2.0.0", 'b')), 1000);
        // notify (the default): nothing happens on its own
        f.up.sweep().await;
        assert!(f.node.staged_upgrades().is_empty());
        f.store
            .transaction(|tx| store_nodes::set_upgrade_default(tx, "auto", 1))
            .unwrap();
        f.node.set_upgrade_outcome("rolled_back", Some("no link"));
        f.up.sweep().await;
        let first = f
            .store
            .reader(|c| store_nodes::upgrade_latest(c, "n_a"))
            .unwrap()
            .unwrap();
        ended(&f, &first.operation_id).await;
        f.up.sweep().await;
        assert_eq!(
            f.node.staged_upgrades().len(),
            1,
            "a failed version is not retried"
        );
        // a node's own policy wins over the hub's
        f.store
            .transaction(|tx| store_nodes::set_upgrade_policy(tx, "n_a", "u", Some("off")))
            .unwrap();
        let row = f
            .store
            .reader(|c| store_nodes::get(c, "n_a"))
            .unwrap()
            .unwrap();
        assert_eq!(row.upgrade_policy.as_deref(), Some("off"));
        assert!(
            !f.store
                .transaction(|tx| store_nodes::set_upgrade_policy(tx, "n_a", "u", Some("always")))
                .unwrap()
        );
    }

    #[tokio::test]
    async fn the_archive_is_read_only_by_its_node_under_its_operation_while_it_stages() {
        let f = fixture(true, Some(build("2.0.0", 'b')), 1000);
        let dir = f.data.join("node-upgrades");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.tar.gz");
        std::fs::write(&file, b"0123456789").unwrap();
        let row = UpgradeRow {
            operation_id: "nup_x".into(),
            node_id: "n_a".into(),
            version: "2.0.0".into(),
            commit_sha: "b".repeat(40),
            target: TARGET.into(),
            from_version: None,
            artifact: format!("{ARTIFACT_PREFIX}nup_x"),
            file: file.display().to_string(),
            sha256: String::new(),
            size: 10,
            requested_by: "u".into(),
            force: false,
            phase: "staging".into(),
            error: None,
            created: 1,
            updated: 1,
        };
        f.store
            .transaction(|tx| store_nodes::upgrade_insert(tx, &row))
            .unwrap();
        let (n, op, id) = (
            NodeId::new("n_a").unwrap(),
            OperationId::new("nup_x").unwrap(),
            ArtifactId::new(row.artifact.clone()).unwrap(),
        );
        let c = read_chunk(&f.store, &f.data, &n, &op, &id, 4, 4).unwrap();
        assert_eq!((c.bytes.as_slice(), c.eof), (&b"4567"[..], false));
        let other = NodeId::new("n_b").unwrap();
        assert_eq!(
            read_chunk(&f.store, &f.data, &other, &op, &id, 0, 4)
                .unwrap_err()
                .code,
            ErrorCode::Denied
        );
        let wrong_op = OperationId::new("nup_y").unwrap();
        assert!(read_chunk(&f.store, &f.data, &n, &wrong_op, &id, 0, 4).is_err());
        f.store
            .transaction(|tx| store_nodes::upgrade_phase(tx, "nup_x", "staged", None, 2))
            .unwrap();
        assert_eq!(
            read_chunk(&f.store, &f.data, &n, &op, &id, 0, 4)
                .unwrap_err()
                .code,
            ErrorCode::Denied,
            "not after it staged"
        );
    }
}
