//! Removing a Node installed as a service (`steve node install`) from its machine, when its Hub
//! asks over the Node's own link. Nothing is uninstalled or deleted before the Hub confirms.
//!
//! 1. `begin` ([`NodeRemove`]): the Node takes no new work at once ([`LocalNode::set_retiring`])
//!    and waits for what runs to end (`draining`). Past the grace period the Hub named, it cancels
//!    what still runs (`cancelling`) and waits up to [`RemoveTiming::cancel_wait`] for it to end.
//!    Then it is `ready`, and waits up to [`RemoveTiming::ready_wait`] for the Hub; without an
//!    answer it gives the removal up and takes work again (`abandoned`).
//! 2. `commit` ([`RemoveCommit`]): the Hub recorded that the Node is ready and confirms. The Node
//!    writes `removing`, starts a helper outside its service (`steve-node remove-finish <op>`, this
//!    binary), answers, and drops its link. The helper ([`run_remove`]) stops the service,
//!    uninstalls it (what `steve node uninstall` does) and deletes the Node's home. It refuses to
//!    touch anything unless the record says `removing`.
//!
//! Every step is written to `<home>/updates/remove/<op>.json` before it is taken; asking again
//! answers what is written. A Node that restarts in the middle gives the removal up.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use steve_node::{LocalNode, Remover};
use steve_node_api::*;

/// Where the removal records are, under the Node's home.
pub const DIR: &str = "updates/remove";
/// The longest grace period a Hub may ask for: a day.
const GRACE_MAX: u32 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    state: NodeRemoveState,
    request: NodeRemove,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn err(code: Code, m: impl Into<String>) -> Error {
    Error::new(code, m)
}

fn io_err(e: impl std::fmt::Display) -> Error {
    err(Code::Internal, e.to_string())
}

/// A removal's operation names its record file: letters, digits, `_` and `-` only.
fn op_name(op: &OperationId) -> ApiResult<&str> {
    let s = op.as_str();
    if s.is_empty()
        || s.len() > 96
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(err(Code::Invalid, "bad removal operation id"));
    }
    Ok(s)
}

fn record_path(home: &Path, op: &str) -> PathBuf {
    home.join(DIR).join(format!("{op}.json"))
}

fn load(home: &Path, op: &str) -> std::io::Result<Option<Record>> {
    match std::fs::read(record_path(home, op)) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(std::io::Error::other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn save(home: &Path, r: &Record) -> std::io::Result<()> {
    let dir = home.join(DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = record_path(home, r.state.operation.as_str());
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    std::fs::File::open(&dir)?.sync_all()
}

fn records(home: &Path) -> Vec<Record> {
    let Ok(dir) = std::fs::read_dir(home.join(DIR)) else {
        return vec![];
    };
    dir.filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect()
}

fn set_phase(home: &Path, r: &mut Record, phase: &str, running: usize, error: Option<String>) {
    r.state.phase = phase.into();
    r.state.running = running.min(u32::MAX as usize) as u32;
    r.state.error = error;
    r.state.updated = Timestamp(now());
    if let Err(e) = save(home, r) {
        tracing::error!(
            "node removal {}: writing {phase}: {e}",
            r.state.operation.as_str()
        );
    }
}

/// What a removal needs of the Node it removes ([`LocalNode`] in production).
#[async_trait]
pub trait Work: Send + Sync + 'static {
    /// Turns, jobs and commands running now.
    fn running(&self) -> usize;
    fn set_retiring(&self, on: bool);
    /// Cancels what runs; how many it asked to stop.
    async fn cancel_running(&self, reason: &str) -> usize;
}

#[async_trait]
impl Work for LocalNode {
    fn running(&self) -> usize {
        // a journal that cannot say counts as busy: the grace period then ends it
        self.running_work().unwrap_or(1)
    }
    fn set_retiring(&self, on: bool) {
        LocalNode::set_retiring(self, on)
    }
    async fn cancel_running(&self, reason: &str) -> usize {
        match LocalNode::cancel_running(self, reason).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("cancelling running work for the removal: {e}");
                0
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RemoveTiming {
    /// How often a removal looks at what runs.
    pub poll: Duration,
    /// After cancelling, how long the Node waits for what it cancelled to end before it is ready
    /// anyway (stopping the service ends the rest).
    pub cancel_wait: Duration,
    /// How long a ready removal waits for the Hub's confirmation before the Node gives it up.
    pub ready_wait: Duration,
    /// After the confirmation is answered, how long before the Node drops its link.
    pub drop_after: Duration,
}

impl Default for RemoveTiming {
    fn default() -> RemoveTiming {
        RemoveTiming {
            poll: Duration::from_secs(1),
            cancel_wait: Duration::from_secs(60),
            ready_wait: Duration::from_secs(10 * 60),
            drop_after: Duration::from_secs(1),
        }
    }
}

/// Starts the helper that finishes a confirmed removal (`home`, operation).
pub type Spawner = Box<dyn Fn(&Path, &str) -> std::io::Result<()> + Send + Sync>;

/// A Node installed as a service, running from the release its service record names, can be
/// removed from its machine by its Hub.
pub struct ServiceRemover {
    home: PathBuf,
    timing: RemoveTiming,
    spawn: Spawner,
    /// Taken around every read-change-write of a record (never across an await).
    lock: Arc<Mutex<()>>,
    /// Told once a confirmed removal answered: the link goes ([`ServiceRemover::dropped`]).
    dropped: Arc<tokio::sync::Notify>,
}

impl ServiceRemover {
    /// `None` when this process is not the service's (run by hand, another release than the
    /// installed one).
    pub fn detect(home: &Path) -> Option<Arc<ServiceRemover>> {
        crate::upgrade::service_release(home)?;
        Some(Arc::new(ServiceRemover::new(
            home,
            RemoveTiming::default(),
            Box::new(spawn_helper),
        )))
    }

    /// A remover for the Node in `home`; what a restart left unfinished is given up.
    pub fn new(home: &Path, timing: RemoveTiming, spawn: Spawner) -> ServiceRemover {
        settle(home);
        ServiceRemover {
            home: home.to_owned(),
            timing,
            spawn,
            lock: Arc::new(Mutex::new(())),
            dropped: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Resolves once a confirmed removal answered the Hub: the Node then drops its link and waits
    /// for the helper to stop its service.
    pub fn dropped(&self) -> Arc<tokio::sync::Notify> {
        self.dropped.clone()
    }

    pub async fn begin_with(
        &self,
        work: Arc<dyn Work>,
        r: NodeRemove,
    ) -> ApiResult<NodeRemoveState> {
        let op = op_name(&r.operation)?.to_string();
        if r.grace_secs > GRACE_MAX {
            return Err(err(Code::Invalid, "the grace period is too long"));
        }
        let record = {
            let _g = self.lock.lock().unwrap();
            if let Some(old) = load(&self.home, &op).map_err(io_err)? {
                if old.request != r {
                    return Err(err(Code::Conflict, "another removal under this operation"));
                }
                return Ok(old.state);
            }
            if records(&self.home).iter().any(|x| !x.state.is_terminal()) {
                return Err(err(Code::Busy, "another removal is unfinished"));
            }
            work.set_retiring(true);
            let record = Record {
                state: NodeRemoveState {
                    operation: r.operation.clone(),
                    phase: "draining".into(),
                    running: work.running().min(u32::MAX as usize) as u32,
                    error: None,
                    updated: Timestamp(now()),
                },
                request: r,
            };
            if let Err(e) = save(&self.home, &record) {
                work.set_retiring(false);
                return Err(io_err(e));
            }
            record
        };
        tracing::info!(op, "node removal: taking no new work");
        let state = record.state.clone();
        let (home, lock, timing) = (self.home.clone(), self.lock.clone(), self.timing.clone());
        tokio::spawn(async move { drive(&home, &lock, &op, work, &timing).await });
        Ok(state)
    }

    pub async fn commit_with(
        &self,
        work: Arc<dyn Work>,
        c: RemoveCommit,
    ) -> ApiResult<NodeRemoveState> {
        let op = op_name(&c.operation)?.to_string();
        let state = {
            let _g = self.lock.lock().unwrap();
            let mut r = load(&self.home, &op)
                .map_err(io_err)?
                .ok_or_else(|| err(Code::Invalid, "no such removal"))?;
            if r.state.is_terminal() {
                return Ok(r.state);
            }
            let running = r.state.running as usize;
            if c.abandon {
                set_phase(&self.home, &mut r, "abandoned", running, None);
                work.set_retiring(false);
                tracing::info!(op, "node removal given up; taking work again");
                return Ok(r.state);
            }
            if r.state.phase != "ready" {
                return Err(err(
                    Code::Busy,
                    format!("the node still runs {running} turns or commands"),
                ));
            }
            set_phase(&self.home, &mut r, "removing", running, None);
            if let Err(e) = (self.spawn)(&self.home, &op) {
                set_phase(
                    &self.home,
                    &mut r,
                    "failed",
                    running,
                    Some(format!("starting the removal: {e}")),
                );
                work.set_retiring(false);
                return Ok(r.state);
            }
            r.state
        };
        tracing::info!(op, "node removal confirmed; dropping the link");
        // the answer goes out first
        let (dropped, after) = (self.dropped.clone(), self.timing.drop_after);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            dropped.notify_one();
        });
        Ok(state)
    }

    pub fn status_of(&self, operation: &OperationId) -> ApiResult<NodeRemoveState> {
        let op = op_name(operation)?;
        load(&self.home, op)
            .map_err(io_err)?
            .map(|r| r.state)
            .ok_or_else(|| err(Code::Invalid, "no such removal"))
    }
}

#[async_trait]
impl Remover for ServiceRemover {
    async fn begin(&self, node: &LocalNode, r: NodeRemove) -> ApiResult<NodeRemoveState> {
        self.begin_with(Arc::new(node.clone()), r).await
    }
    async fn commit(&self, node: &LocalNode, c: RemoveCommit) -> ApiResult<NodeRemoveState> {
        self.commit_with(Arc::new(node.clone()), c).await
    }
    fn status(&self, operation: &OperationId) -> ApiResult<NodeRemoveState> {
        self.status_of(operation)
    }
}

/// A removal the Node restarted in the middle of: given up (the new process takes work).
fn settle(home: &Path) {
    for mut r in records(home) {
        if matches!(r.state.phase.as_str(), "draining" | "cancelling" | "ready") {
            set_phase(
                home,
                &mut r,
                "abandoned",
                0,
                Some("the node restarted during the removal".into()),
            );
        }
    }
}

/// Changes the record `op` to `phase` unless it moved on meanwhile (given up, confirmed): whether
/// it did.
fn advance(
    home: &Path,
    lock: &Mutex<()>,
    op: &str,
    from: &[&str],
    phase: &str,
    running: usize,
    error: Option<String>,
) -> bool {
    let _g = lock.lock().unwrap();
    match load(home, op) {
        Ok(Some(mut r)) if from.contains(&r.state.phase.as_str()) => {
            if r.state.phase != phase || r.state.running as usize != running {
                set_phase(home, &mut r, phase, running, error);
            }
            true
        }
        _ => false,
    }
}

/// Waits for what runs to end, cancels it past the grace period, then waits for the Hub.
async fn drive(home: &Path, lock: &Mutex<()>, op: &str, work: Arc<dyn Work>, t: &RemoveTiming) {
    let Ok(Some(r)) = load(home, op) else { return };
    let grace = Duration::from_secs(r.request.grace_secs as u64);
    let started = Instant::now();
    let mut cancelled: Option<Instant> = None;
    loop {
        let running = work.running();
        if running == 0 {
            if !advance(
                home,
                lock,
                op,
                &["draining", "cancelling"],
                "ready",
                0,
                None,
            ) {
                return;
            }
            break;
        }
        if cancelled.is_none() && started.elapsed() >= grace {
            tracing::info!(op, running, "node removal: grace period over, cancelling");
            work.cancel_running("the node is being removed from its machine")
                .await;
            cancelled = Some(Instant::now());
            if !advance(home, lock, op, &["draining"], "cancelling", running, None) {
                return;
            }
        } else if cancelled.is_some_and(|c| c.elapsed() >= t.cancel_wait) {
            let why = format!(
                "{running} turns or commands still ran after they were cancelled; stopping the service ends them"
            );
            if !advance(home, lock, op, &["cancelling"], "ready", running, Some(why)) {
                return;
            }
            break;
        } else if !advance(
            home,
            lock,
            op,
            &["draining", "cancelling"],
            draining_phase(cancelled.is_some()),
            running,
            None,
        ) {
            return;
        }
        tokio::time::sleep(t.poll).await;
    }
    tracing::info!(op, "node removal: ready, waiting for the hub");
    let until = Instant::now() + t.ready_wait;
    loop {
        tokio::time::sleep(t.poll).await;
        if Instant::now() < until {
            if removal_is_ready(home, op) {
                continue;
            }
            return;
        }
        let why = format!(
            "the hub did not confirm within {} s; the node takes work again",
            t.ready_wait.as_secs()
        );
        if advance(home, lock, op, &["ready"], "abandoned", 0, Some(why)) {
            work.set_retiring(false);
        }
        return;
    }
}

/// Continue waiting only while the persisted removal still belongs to the ready phase.
fn removal_is_ready(home: &Path, op: &str) -> bool {
    matches!(load(home, op), Ok(Some(r)) if r.state.phase == "ready")
}

/// The ongoing drain's persisted phase, without changing its timers or transitions.
fn draining_phase(cancelled: bool) -> &'static str {
    if cancelled { "cancelling" } else { "draining" }
}

/// `steve-node remove-finish <op>`, detached from the service so stopping it does not stop the
/// helper.
fn spawn_helper(home: &Path, op: &str) -> std::io::Result<()> {
    crate::upgrade::spawn_detached(home, &["remove-finish", op], &format!("remove-{op}"))
}

/// What the helper does to the service, as `steve node stop|uninstall` do it.
pub trait RemoveOps {
    fn stop(&mut self) -> anyhow::Result<()>;
    fn uninstall(&mut self) -> anyhow::Result<()>;
}

impl RemoveOps for crate::upgrade::InstalledService {
    fn stop(&mut self) -> anyhow::Result<()> {
        self.0.dispatch("stop", None, 0).map(|_| ())
    }
    fn uninstall(&mut self) -> anyhow::Result<()> {
        self.0.dispatch("uninstall", None, 0).map(|_| ())
    }
}

/// `home` is a Node's own home, and only that: absolute, with its service record, and neither the
/// file system's root nor the user's home directory.
fn check_home(home: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    anyhow::ensure!(
        home.is_absolute(),
        "the node's home is not an absolute path"
    );
    let home = home.canonicalize().context("the node's home")?;
    anyhow::ensure!(
        home.parent().is_some(),
        "refusing to delete the file system's root"
    );
    if let Some(user) = std::env::var_os("HOME")
        && let Ok(user) = Path::new(&user).canonicalize()
    {
        anyhow::ensure!(home != user, "refusing to delete the user's home directory");
    }
    anyhow::ensure!(
        home.join("services/node.json").is_file(),
        "{} holds no node service record; nothing deleted",
        home.display()
    );
    Ok(home)
}

/// The helper's whole job, for the record `op` in `removing` (the Hub confirmed): stop the
/// service, uninstall it, delete the home. `pause`: how long it first lets the Node's answer and
/// the end of its link reach the Hub; `retry`: how often it tries the uninstall again while the
/// service manager lets the stopped service go (at most 30 times).
pub fn run_remove(
    home: &Path,
    op: &str,
    svc: &mut dyn RemoveOps,
    pause: Duration,
    retry: Duration,
) -> anyhow::Result<()> {
    use anyhow::Context;
    let mut r = load(home, op)?.context("no such removal")?;
    anyhow::ensure!(
        r.state.phase == "removing",
        "the removal is {}, not confirmed by the hub; nothing was changed",
        r.state.phase
    );
    let home = check_home(home)?;
    std::thread::sleep(pause);
    stop_removal_service(&home, op, &mut r, svc)?;
    uninstall_removal_service(&home, op, &mut r, svc, retry)?;
    tracing::info!("node removal {op}: deleting {}", home.display());
    std::fs::remove_dir_all(&home).with_context(|| format!("deleting {}", home.display()))?;
    Ok(())
}

/// Stop the confirmed Node service, recording a refusal before returning its error.
fn stop_removal_service(
    home: &Path,
    op: &str,
    r: &mut Record,
    svc: &mut dyn RemoveOps,
) -> anyhow::Result<()> {
    tracing::info!("node removal {op}: stopping the service");
    if let Err(e) = svc.stop() {
        set_phase(
            home,
            r,
            "failed",
            0,
            Some(format!("stopping the node: {e:#}")),
        );
        return Err(e.context("stopping the node"));
    }
    Ok(())
}

/// Retry the service-manager uninstall under the existing retry budget.
fn uninstall_removal_service(
    home: &Path,
    op: &str,
    r: &mut Record,
    svc: &mut dyn RemoveOps,
    retry: Duration,
) -> anyhow::Result<()> {
    tracing::info!("node removal {op}: uninstalling the service");
    let mut tries = 0;
    loop {
        match svc.uninstall() {
            Ok(()) => break,
            Err(e) if tries < 30 => {
                tries += 1;
                tracing::info!("node removal {op}: uninstall not yet possible: {e:#}");
                std::thread::sleep(retry);
            }
            Err(e) => {
                set_phase(
                    home,
                    r,
                    "failed",
                    0,
                    Some(format!("uninstalling the service: {e:#}")),
                );
                return Err(e.context("uninstalling the service"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A Node whose running work the test sets; it records what the removal asked of it.
    #[derive(Default)]
    struct FakeWork {
        running: AtomicUsize,
        retiring: AtomicBool,
        cancels: AtomicUsize,
        /// Cancelling ends what runs.
        cancel_ends: AtomicBool,
    }

    #[async_trait]
    impl Work for FakeWork {
        fn running(&self) -> usize {
            self.running.load(Ordering::SeqCst)
        }
        fn set_retiring(&self, on: bool) {
            self.retiring.store(on, Ordering::SeqCst);
        }
        async fn cancel_running(&self, _reason: &str) -> usize {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            if self.cancel_ends.load(Ordering::SeqCst) {
                self.running.store(0, Ordering::SeqCst)
            }
            self.running()
        }
    }

    fn timing() -> RemoveTiming {
        RemoveTiming {
            poll: Duration::from_millis(10),
            cancel_wait: Duration::from_millis(100),
            ready_wait: Duration::from_secs(30),
            drop_after: Duration::from_millis(10),
        }
    }

    /// A Node's home as `steve node install` leaves it, with something in it.
    fn node_home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path();
        std::fs::create_dir_all(h.join("services")).unwrap();
        std::fs::write(h.join("services/node.json"), b"{\"component\":\"node\"}").unwrap();
        std::fs::create_dir_all(h.join("node/local")).unwrap();
        std::fs::write(h.join("node/identity.key"), b"seed").unwrap();
        dir
    }

    /// The helpers started, by operation.
    fn remover(home: &Path, t: RemoveTiming) -> (ServiceRemover, Arc<Mutex<Vec<String>>>) {
        let spawned = Arc::new(Mutex::new(vec![]));
        let s = spawned.clone();
        let r = ServiceRemover::new(
            home,
            t,
            Box::new(move |_, op| {
                s.lock().unwrap().push(op.to_string());
                Ok(())
            }),
        );
        (r, spawned)
    }

    fn ask(op: &str, grace_secs: u32) -> NodeRemove {
        NodeRemove {
            operation: OperationId::new(op).unwrap(),
            grace_secs,
        }
    }

    fn commit(op: &str, abandon: bool) -> RemoveCommit {
        RemoveCommit {
            operation: OperationId::new(op).unwrap(),
            abandon,
        }
    }

    async fn phase_becomes(r: &ServiceRemover, op: &str, phase: &str) -> NodeRemoveState {
        let id = OperationId::new(op).unwrap();
        for _ in 0..500 {
            let s = r.status_of(&id).unwrap();
            if s.phase == phase {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{op} never became {phase}: {:?}", r.status_of(&id));
    }

    /// The service as a test drives it: what it was asked, in order.
    #[derive(Default)]
    struct FakeService {
        calls: Vec<&'static str>,
        /// The uninstall fails this many times first (the service manager has not let go yet).
        busy: usize,
        fail_stop: bool,
    }

    impl RemoveOps for FakeService {
        fn stop(&mut self) -> anyhow::Result<()> {
            self.calls.push("stop");
            anyhow::ensure!(!self.fail_stop, "launchctl failed");
            Ok(())
        }
        fn uninstall(&mut self) -> anyhow::Result<()> {
            self.calls.push("uninstall");
            if self.busy > 0 {
                self.busy -= 1;
                anyhow::bail!("service is running, loaded, or its state is unknown");
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn an_idle_node_stops_taking_work_is_ready_and_starts_the_helper_only_once_confirmed() {
        let home = node_home();
        let (r, spawned) = remover(home.path(), timing());
        let work = Arc::new(FakeWork::default());
        let s = r.begin_with(work.clone(), ask("nrm_1", 600)).await.unwrap();
        assert_eq!(s.phase, "draining");
        assert!(
            work.retiring.load(Ordering::SeqCst),
            "no new work from now on"
        );
        phase_becomes(&r, "nrm_1", "ready").await;
        assert!(
            spawned.lock().unwrap().is_empty(),
            "nothing before the hub confirms"
        );
        // asked again: its state, never a second removal
        assert_eq!(
            r.begin_with(work.clone(), ask("nrm_1", 600))
                .await
                .unwrap()
                .phase,
            "ready"
        );
        assert_eq!(
            r.begin_with(work.clone(), ask("nrm_2", 600))
                .await
                .unwrap_err()
                .code,
            Code::Busy
        );
        let dropped = r.dropped();
        let s = r
            .commit_with(work.clone(), commit("nrm_1", false))
            .await
            .unwrap();
        assert_eq!(s.phase, "removing");
        assert_eq!(*spawned.lock().unwrap(), vec!["nrm_1".to_string()]);
        tokio::time::timeout(Duration::from_secs(5), dropped.notified())
            .await
            .expect("the link is dropped once the confirmation is answered");
        // confirmed twice: the same answer, one helper
        r.commit_with(work.clone(), commit("nrm_1", false))
            .await
            .unwrap();
        assert_eq!(spawned.lock().unwrap().len(), 1);
        assert!(
            home.path().join("services/node.json").exists(),
            "the helper deletes, not the node"
        );
    }

    #[tokio::test]
    async fn running_work_is_waited_for_then_cancelled_past_the_grace_period() {
        let home = node_home();
        let (r, spawned) = remover(home.path(), timing());
        let work = Arc::new(FakeWork::default());
        work.running.store(2, Ordering::SeqCst);
        work.cancel_ends.store(true, Ordering::SeqCst);
        r.begin_with(work.clone(), ask("nrm_g", 0)).await.unwrap();
        let s = phase_becomes(&r, "nrm_g", "ready").await;
        assert_eq!(work.cancels.load(Ordering::SeqCst), 1, "cancelled once");
        assert_eq!(s.running, 0);
        assert!(spawned.lock().unwrap().is_empty());

        // what does not end after it was cancelled: ready anyway, saying so
        let home = node_home();
        let (r, _) = remover(home.path(), timing());
        let work = Arc::new(FakeWork::default());
        work.running.store(1, Ordering::SeqCst);
        r.begin_with(work.clone(), ask("nrm_s", 0)).await.unwrap();
        let s = phase_becomes(&r, "nrm_s", "ready").await;
        assert_eq!(s.running, 1);
        assert!(s.error.unwrap().contains("still ran"));
    }

    #[tokio::test]
    async fn a_draining_node_refuses_to_be_confirmed_and_an_abandoned_one_takes_work_again() {
        let home = node_home();
        let (r, spawned) = remover(home.path(), timing());
        let work = Arc::new(FakeWork::default());
        work.running.store(1, Ordering::SeqCst);
        r.begin_with(work.clone(), ask("nrm_d", 600)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            r.commit_with(work.clone(), commit("nrm_d", false))
                .await
                .unwrap_err()
                .code,
            Code::Busy
        );
        let s = r
            .commit_with(work.clone(), commit("nrm_d", true))
            .await
            .unwrap();
        assert_eq!(s.phase, "abandoned");
        assert!(!work.retiring.load(Ordering::SeqCst), "takes work again");
        // the drive sees it was given up and leaves it so
        work.running.store(0, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            r.status_of(&OperationId::new("nrm_d").unwrap())
                .unwrap()
                .phase,
            "abandoned"
        );
        assert!(spawned.lock().unwrap().is_empty());
        assert!(home.path().join("services/node.json").exists());
    }

    #[tokio::test]
    async fn a_ready_node_the_hub_never_confirms_gives_it_up() {
        let home = node_home();
        let mut t = timing();
        t.ready_wait = Duration::from_millis(100);
        let (r, spawned) = remover(home.path(), t);
        let work = Arc::new(FakeWork::default());
        r.begin_with(work.clone(), ask("nrm_w", 600)).await.unwrap();
        let s = phase_becomes(&r, "nrm_w", "abandoned").await;
        assert!(s.error.unwrap().contains("did not confirm"));
        assert!(!work.retiring.load(Ordering::SeqCst));
        assert!(spawned.lock().unwrap().is_empty());
    }

    #[test]
    fn a_restart_gives_up_what_it_left_unfinished() {
        let home = node_home();
        for (op, phase) in [
            ("nrm_a", "draining"),
            ("nrm_b", "ready"),
            ("nrm_c", "removing"),
        ] {
            let mut r = Record {
                state: NodeRemoveState {
                    operation: OperationId::new(op).unwrap(),
                    phase: phase.into(),
                    running: 0,
                    error: None,
                    updated: Timestamp(now()),
                },
                request: ask(op, 60),
            };
            set_phase(home.path(), &mut r, phase, 0, None);
        }
        let (r, _) = remover(home.path(), timing());
        let phase = |op: &str| r.status_of(&OperationId::new(op).unwrap()).unwrap().phase;
        assert_eq!(phase("nrm_a"), "abandoned");
        assert_eq!(phase("nrm_b"), "abandoned");
        assert_eq!(phase("nrm_c"), "removing", "confirmed: the helper's");
    }

    fn record(home: &Path, op: &str, phase: &str) {
        let mut r = Record {
            state: NodeRemoveState {
                operation: OperationId::new(op).unwrap(),
                phase: phase.into(),
                running: 0,
                error: None,
                updated: Timestamp(now()),
            },
            request: ask(op, 60),
        };
        set_phase(home, &mut r, phase, 0, None);
    }

    #[test]
    fn the_helper_changes_nothing_before_the_hub_confirmed() {
        for phase in ["draining", "cancelling", "ready", "abandoned"] {
            let home = node_home();
            record(home.path(), "nrm_1", phase);
            let mut svc = FakeService::default();
            let e = run_remove(
                home.path(),
                "nrm_1",
                &mut svc,
                Duration::ZERO,
                Duration::ZERO,
            )
            .unwrap_err();
            assert!(format!("{e:#}").contains("not confirmed"), "{phase}: {e:#}");
            assert!(svc.calls.is_empty(), "{phase}: the service is not touched");
            assert!(
                home.path().join("node/identity.key").exists(),
                "{phase}: the home stays"
            );
        }
        // no record at all: nothing either
        let home = node_home();
        let mut svc = FakeService::default();
        assert!(
            run_remove(
                home.path(),
                "nrm_x",
                &mut svc,
                Duration::ZERO,
                Duration::ZERO
            )
            .is_err()
        );
        assert!(svc.calls.is_empty());
    }

    #[test]
    fn a_confirmed_removal_stops_and_uninstalls_the_service_then_deletes_the_home() {
        let home = node_home();
        record(home.path(), "nrm_1", "removing");
        // launchd lets the stopped job go a moment later: the uninstall is tried again
        let mut svc = FakeService {
            busy: 2,
            ..Default::default()
        };
        run_remove(
            home.path(),
            "nrm_1",
            &mut svc,
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(
            svc.calls,
            vec!["stop", "uninstall", "uninstall", "uninstall"]
        );
        assert!(!home.path().exists(), "the home is gone");
    }

    #[test]
    fn a_service_that_does_not_stop_keeps_the_home_and_says_why() {
        let home = node_home();
        record(home.path(), "nrm_1", "removing");
        let mut svc = FakeService {
            fail_stop: true,
            ..Default::default()
        };
        assert!(
            run_remove(
                home.path(),
                "nrm_1",
                &mut svc,
                Duration::ZERO,
                Duration::ZERO
            )
            .is_err()
        );
        assert_eq!(svc.calls, vec!["stop"], "never uninstalled");
        assert!(home.path().join("node/identity.key").exists());
        let s = load(home.path(), "nrm_1").unwrap().unwrap().state;
        assert_eq!(s.phase, "failed");
        assert!(s.error.unwrap().contains("stopping the node"));
    }

    #[test]
    fn only_a_nodes_own_home_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        // no service record: not a Node's home
        record(dir.path(), "nrm_1", "removing");
        let mut svc = FakeService::default();
        let e = run_remove(
            dir.path(),
            "nrm_1",
            &mut svc,
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("no node service record"), "{e:#}");
        assert!(svc.calls.is_empty());
        assert!(dir.path().exists());
        assert!(check_home(Path::new("relative/home")).is_err());
        assert!(op_name(&OperationId::new("..").unwrap()).is_err());
        assert!(op_name(&OperationId::new("a:b").unwrap()).is_err());
    }
}
