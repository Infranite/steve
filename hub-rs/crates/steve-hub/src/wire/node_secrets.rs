//! Vault uses on the Node a session works on (N0e-E7), and the Hub's side of the secrets channel
//! ([`HubSecrets`]).
//!
//! [`VaultNodes`] is the vault's [`UseNodes`]: a session bound to a Node runs its approved command
//! in its workspace there; one that is not (a legacy session) in its directory, prepared as a
//! workspace of that session and directory. The command is written to the Node's ledger under
//! `vault_use:<use>` before it is sent and is never sent twice. Its secrets are references only
//! ([`SecretRef`], the field's label as the id): what the ledger, the Node's journal and every log
//! hold. With the Hub's local Node off nothing here is used ([`UseNodes::place`] answers `None`).
//!
//! [`NodeSecrets`] holds what each such command may take, by operation: the Node takes each value
//! once, right before the command runs; it is decrypted then and nowhere kept. A second take is
//! `stale`, a take for another operation or by another Node `denied`, and so is anything after the
//! command ended or after a restart (nothing survives one: a command the ledger sends after a
//! restart gets no value and does not run).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use steve_modules::vault::inject::{How, RunOutput};
use steve_modules::vault::{NodePlace, NodeUse, UseNodes, UseValues, VaultResult, pasted_secret};
use steve_node_api::*;
use steve_store::{Store, StoreError};

use super::flow_cmd::{dir_digest, prepare_dir};
use super::workspaces::{At, PlacementError, Subject, Workspaces};

/// What each stream of a command keeps (as the vault's own run: 64 KiB).
const OUTPUT_CAP: u64 = 64 * 1024;
/// Past the command's own deadline, how long its answer may take to come back.
const ANSWER_GRACE: Duration = Duration::from_secs(60);
/// Operations whose secrets are over, remembered so a late take is `stale`, not `denied`.
const ENDED_KEPT: usize = 1024;
/// A use whose answer was lost (a restart, a lost link): it may have run, and is not run again.
const UNKNOWN: &str = "命令的结果未知：它可能已经运行过，不会再运行一次";
/// The session's Node may not get files (any but the Hub's own machine).
const NO_FILES: &str = "这台设备不接受以文件方式交付的机密";
/// The session's Node runs no commands.
const NO_EXECS: &str = "这台设备不运行命令，不能在它上面使用机密";
/// A command with a secret written into it: the ledger and the Node's journal keep the command as
/// it is (the vault refuses it when asked; this is the last stop before either).
const PASTED: &str = "命令里有像机密的内容，不会在设备上运行；请改用 env 或 stdin 注入";
/// As the vault says it (`modules.vault.cwd_outside`, zh-CN: a tool result).
const CWD_OUTSIDE: &str = "目录不在这个会话的工作区里";

/// One command's secrets.
struct Held {
    node: NodeId,
    /// The ids it may take (the use's field labels), and those it took.
    ids: Vec<String>,
    taken: HashSet<String>,
    values: Arc<dyn UseValues>,
}

/// What every Node may take now, by operation. One per Hub.
#[derive(Default)]
pub struct NodeSecrets {
    held: Mutex<HashMap<String, Held>>,
    ended: Mutex<VecDeque<String>>,
    /// Push grants being observed; one observer per operation.
    watching: Mutex<HashSet<String>>,
}

impl NodeSecrets {
    pub fn new() -> Arc<NodeSecrets> {
        Arc::default()
    }

    /// The channel of one Node: it takes only what was given to it.
    pub fn for_node(self: &Arc<Self>, node: NodeId) -> Arc<dyn HubSecrets> {
        Arc::new(Bound {
            all: self.clone(),
            node,
        })
    }

    /// `op`, a command for `node`, may take `ids` once each, from `values`.
    pub(super) fn give(
        &self,
        op: &str,
        node: NodeId,
        ids: Vec<String>,
        values: Arc<dyn UseValues>,
    ) {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).insert(
            op.into(),
            Held {
                node,
                ids,
                taken: HashSet::new(),
                values,
            },
        );
    }

    /// A push asked about again keeps its original grant, including what it already took.
    /// An ended operation cannot acquire a new grant in this Hub process.
    pub(super) fn give_once(
        &self,
        op: &str,
        node: NodeId,
        ids: Vec<String>,
        values: Arc<dyn UseValues>,
        may_grant: impl FnOnce() -> Result<bool, StoreError>,
    ) -> Result<(), StoreError> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        self.give_once_in(&mut held, op, node, ids, values, may_grant)?;
        Ok(())
    }

    /// Publish the grant and the durable command together. A failed submission can withdraw
    /// only the grant it just created: take/retirement and other submissions cannot pass this
    /// lock before the synchronous submission returns.
    pub(super) fn submit_push<T>(
        &self,
        op: &str,
        node: NodeId,
        ids: Vec<String>,
        values: Arc<dyn UseValues>,
        may_grant: impl FnOnce() -> Result<bool, StoreError>,
        submit: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let supplied = self.give_once_in(&mut held, op, node, ids, values, may_grant)?;
        let result = submit();
        if result.is_err() && supplied {
            held.remove(op);
        }
        result
    }

    fn give_once_in(
        &self,
        held: &mut HashMap<String, Held>,
        op: &str,
        node: NodeId,
        ids: Vec<String>,
        values: Arc<dyn UseValues>,
        may_grant: impl FnOnce() -> Result<bool, StoreError>,
    ) -> Result<bool, StoreError> {
        if held.contains_key(op)
            || self
                .ended
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|ended| ended == op)
        {
            return Ok(false);
        }
        // The bounded tombstone cache is diagnostic, not authority. Re-read the ledger while
        // holding the grant lock so an old caller cannot revive a settled operation after
        // its tombstone has been evicted. The callback only reads; it must not call secrets.
        if !may_grant()? {
            return Ok(false);
        }
        held.insert(
            op.into(),
            Held {
                node,
                ids,
                taken: HashSet::new(),
                values,
            },
        );
        Ok(true)
    }

    /// A caller's wait ending or being cancelled does not end a push. Observe its ledger only;
    /// no credential or runtime is kept alive while the observer sleeps.
    pub(super) fn watch_push(
        self: &Arc<Self>,
        runtime: &Arc<super::node_runtime::NodeRuntime>,
        op: &str,
    ) {
        if !self
            .watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(op.into())
        {
            return;
        }
        let secrets = Arc::downgrade(self);
        let runtime = Arc::downgrade(runtime);
        let op = op.to_string();
        tokio::spawn(async move {
            loop {
                let Some(secrets) = secrets.upgrade() else {
                    return;
                };
                let done = match runtime.upgrade() {
                    Some(runtime) => match runtime.ledger(&op) {
                        Ok(Some(entry)) => entry.is_settled(),
                        Ok(None) => true,
                        Err(e) => {
                            tracing::warn!("push {op}: {e}");
                            false
                        }
                    },
                    None => true,
                };
                if done {
                    secrets.retire_push(&op);
                    secrets
                        .watching
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&op);
                    return;
                }
                drop(secrets);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }

    /// `op` ended (or was never sent): nothing of it may be taken any more.
    pub(super) fn end(&self, op: &str) {
        self.end_with_tombstone(op, false);
    }

    /// A push is over even if this Hub has not supplied its grant. A concurrent caller that
    /// read an earlier ledger state must not grant it after retirement.
    pub(super) fn retire_push(&self, op: &str) {
        self.end_with_tombstone(op, true);
    }

    fn end_with_tombstone(&self, op: &str, remember_missing: bool) {
        // Keep the held -> ended lock order shared with take/give_once. Removal and the
        // tombstone must be atomic to a concurrent retry, which must not resurrect the grant.
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if held.remove(op).is_some() || remember_missing {
            let mut ended = self.ended.lock().unwrap_or_else(|e| e.into_inner());
            if !ended.iter().any(|ended| ended == op) {
                ended.push_back(op.into());
            }
            while ended.len() > ENDED_KEPT {
                ended.pop_front();
            }
        }
    }
}

struct Bound {
    all: Arc<NodeSecrets>,
    node: NodeId,
}

#[async_trait]
impl HubSecrets for Bound {
    async fn take(&self, operation: OperationId, id: String) -> ApiResult<SecretBytes> {
        let err = |code, m: &str| Err(ApiError::new(code, m.to_string()).on(&operation));
        let values = {
            let mut held = self.all.held.lock().unwrap_or_else(|e| e.into_inner());
            let Some(h) = held.get_mut(operation.as_str()) else {
                let ended = self.all.ended.lock().unwrap_or_else(|e| e.into_inner());
                return if ended.iter().any(|o| o == operation.as_str()) {
                    err(ErrorCode::Stale, "the operation ended")
                } else {
                    err(ErrorCode::Denied, "no secret for this operation")
                };
            };
            if h.node != self.node {
                return err(ErrorCode::Denied, "a secret of another Node");
            }
            if !h.ids.contains(&id) {
                return err(ErrorCode::Denied, "no such secret for this operation");
            }
            if !h.taken.insert(id.clone()) {
                return err(ErrorCode::Stale, "already taken");
            }
            h.values.clone()
        };
        let label = id.clone();
        let v = tokio::task::spawn_blocking(move || values.value(&label))
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        match v {
            // the vault's bytes go as they are; its copy is wiped when it goes
            Ok(v) => Ok(SecretBytes::new(v.expose().to_vec())),
            Err(e) => {
                tracing::warn!(%operation, "vault use: a value could not be read: {e}");
                err(ErrorCode::Internal, "the value could not be read")
            }
        }
    }
}

/// The vault's uses on Nodes.
pub struct VaultNodes {
    store: Store,
    workspaces: Arc<Workspaces>,
    secrets: Arc<NodeSecrets>,
}

impl VaultNodes {
    pub fn new(
        store: Store,
        workspaces: Arc<Workspaces>,
        secrets: Arc<NodeSecrets>,
    ) -> Arc<VaultNodes> {
        Arc::new(VaultNodes {
            store,
            workspaces,
            secrets,
        })
    }
}

/// The operation of one use: asked again, never run again.
pub fn op_of(use_id: &str) -> String {
    derived_id("op_vuse", use_id)
}

fn failed(error: String) -> RunOutput {
    RunOutput {
        exit_code: None,
        timed_out: false,
        stdout: String::new(),
        stderr: String::new(),
        error: Some(error),
    }
}

fn refs_of(u: &NodeUse) -> Vec<SecretRef> {
    u.injections
        .iter()
        .map(|i| SecretRef {
            id: i.label.clone(),
            how: match i.how {
                How::Env => SecretDelivery::Env {
                    name: i.var.clone(),
                },
                How::File => SecretDelivery::File { env: i.var.clone() },
                How::Stdin => SecretDelivery::Stdin,
            },
        })
        .collect()
}

/// Whether `node` may get a secret as a file.
async fn takes_files(node: &Arc<dyn Node>) -> bool {
    node.probe(false)
        .await
        .is_ok_and(|c| c.features.iter().any(|f| f == FEATURE_SECRET_FILE))
}

/// Whether `node` runs commands at all, and may get a secret as a file.
async fn abilities(node: &Arc<dyn Node>) -> Result<(bool, bool), String> {
    let c = node.probe(false).await.map_err(|e| e.to_string())?;
    Ok((
        c.capacity.execs > 0,
        c.features.iter().any(|f| f == FEATURE_SECRET_FILE),
    ))
}

impl VaultNodes {
    /// The workspace `u` runs in: the session's, or its directory made one.
    async fn workspace_of(
        &self,
        node: &Arc<dyn Node>,
        session: &str,
        at: At,
    ) -> Result<WorkspaceRef, String> {
        match at {
            At::Workspace(w) => Ok(w),
            At::Root(dir) => {
                let resource = format!("session:{session}:{}", dir_digest(&dir));
                prepare_dir(node, resource, dir).await
            }
        }
    }
}

#[async_trait]
impl UseNodes for VaultNodes {
    async fn place(&self, ws: &str, session: &str) -> VaultResult<Option<NodePlace>> {
        let rule = |m: String| -> steve_modules::vault::VaultError {
            steve_modules::ApiError::rule(m).into()
        };
        let in_ws = self
            .store
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(
                    c.query_row(
                        "SELECT device_id FROM sessions WHERE id=? AND workspace_id=?",
                        [session, ws],
                        |r| r.get::<_, String>(0),
                    )
                    .ok(),
                )
            })
            .map_err(|e| rule(e.to_string()))?;
        let Some(device) = in_ws else {
            // the vault answers a session it does not know itself
            return Ok(None);
        };
        // a session's directory made a moment ago may not be among the local roots yet
        self.workspaces
            .refresh_roots()
            .map_err(|e| rule(e.to_string()))?;
        match self.workspaces.placement_of(Subject::Session(session)) {
            Err(PlacementError::LocalOff) => Ok(None),
            Err(e) => Err(rule(e.to_string())),
            Ok(p) => {
                let (execs, files) = abilities(&p.node).await.map_err(rule)?;
                if !execs {
                    return Err(rule(NO_EXECS.into()));
                }
                Ok(Some(NodePlace {
                    device: match &p.at {
                        At::Workspace(_) => p.node_id.as_str().to_string(),
                        At::Root(_) => device,
                    },
                    files,
                }))
            }
        }
    }

    async fn run(&self, u: NodeUse, values: Arc<dyn UseValues>) -> RunOutput {
        if pasted_secret(&u.command) {
            return failed(PASTED.into());
        }
        // the roots may have changed since it was asked (an approval can take a day)
        if let Err(e) = self.workspaces.refresh_roots() {
            return failed(e.to_string());
        }
        let placed = match self.workspaces.placement_of(Subject::Session(&u.session)) {
            Ok(p) => p,
            Err(e) => return failed(e.to_string()),
        };
        let wants_files = u.injections.iter().any(|i| i.how == How::File);
        if wants_files && !takes_files(&placed.node).await {
            return failed(NO_FILES.into());
        }
        let cwd = match u.cwd.as_deref().map(RelPath::new).transpose() {
            Ok(c) => c,
            Err(_) => return failed(CWD_OUTSIDE.into()),
        };
        let workspace = match self
            .workspace_of(&placed.node, &u.session, placed.at.clone())
            .await
        {
            Ok(w) => w,
            Err(e) => return failed(e),
        };
        let deadline = steve_store::now() + u.timeout.as_millis() as i64;
        let spec = ExecSpec {
            workspace,
            command: u.command.clone(),
            cwd,
            policy: ExecutionPolicy {
                revision: Revision::new("vault-use").expect("constant id"),
                permissions: PermissionMode::Ask,
                sandbox: SandboxRequirement::None,
                readable: vec![],
                network: NetworkScope::Unrestricted,
                tools: GrantId::new("hub").expect("constant id"),
            },
            limits: Limits {
                deadline: Timestamp(deadline),
                offline_until: Timestamp(deadline),
                max_output_bytes: OUTPUT_CAP,
                max_tokens: None,
                max_cost: None,
            },
            env: vec![],
            secrets: refs_of(&u),
        };
        let op = op_of(&u.use_id);
        let subject = format!("vault_use:{}", u.use_id);
        let runtime = &placed.runtime;
        let mut ids: Vec<String> = vec![];
        for r in &spec.secrets {
            if !ids.contains(&r.id) {
                ids.push(r.id.clone());
            }
        }
        // given before it can be sent; a use asked again finds its op in the ledger and is not
        // given anything new
        let fresh = match runtime.ledger(&op) {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(e) => return failed(e.to_string()),
        };
        if fresh {
            self.secrets
                .give(&op, placed.node_id.clone(), ids, values.clone());
            if let Err(e) = runtime.submit(&op, &subject, "exec", spec) {
                self.secrets.end(&op);
                return failed(e.to_string());
            }
        }
        let until = Instant::now() + u.timeout + ANSWER_GRACE;
        let out = loop {
            match runtime.ledger(&op) {
                Ok(Some(l)) if l.is_settled() => break ran(&l, u.timeout),
                Ok(Some(_)) => {}
                Ok(None) => break failed(UNKNOWN.into()),
                Err(e) => tracing::warn!("vault use {op}: {e}"),
            }
            if Instant::now() >= until {
                break failed(UNKNOWN.into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        self.secrets.end(&op);
        out
    }
}

/// A settled ledger entry as the use's output (already masked by the Node).
fn ran(l: &super::node_runtime::LedgerEntry, timeout: Duration) -> RunOutput {
    if l.state == "rejected" {
        return failed(
            l.refusal
                .as_ref()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "refused".into()),
        );
    }
    match l.receipt.as_ref().map(|r| &r.state) {
        Some(Observed::Finished {
            outcome: outcome @ (Outcome::Succeeded | Outcome::TimedOut),
            result: Some(OperationResult::Exec(r)),
            ..
        }) => {
            let timed_out = *outcome == Outcome::TimedOut;
            RunOutput {
                exit_code: if timed_out { None } else { r.exit_code },
                timed_out,
                stdout: r.stdout.clone(),
                stderr: r.stderr.clone(),
                // as the vault's own run says it
                error: timed_out.then(|| {
                    format!(
                        "timed out after {} s; the command was stopped",
                        timeout.as_secs()
                    )
                }),
            }
        }
        Some(Observed::Finished {
            outcome: Outcome::Failed,
            error: Some(e),
            ..
        }) => failed(e.to_string()),
        _ => failed(UNKNOWN.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use steve_modules::vault::inject::Injection;
    use steve_modules::vault::secret::SecretBytes as VaultBytes;
    use steve_store::StoreError;

    use super::super::node_runtime::{NodeRuntime, Sink};
    use super::super::node_tools::tests::Ends;
    use super::super::workspaces::LocalFace;
    use super::*;

    const TOKEN: &str = "vlt-T0ken-value-8812";
    const SESSION: &str = "ses_v1";

    /// A use's values: `TOKEN` for every label, counted.
    #[derive(Default)]
    struct Values(AtomicUsize);

    impl UseValues for Values {
        fn value(&self, _label: &str) -> VaultResult<VaultBytes> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(VaultBytes::new(TOKEN.as_bytes().to_vec()))
        }
    }

    struct T {
        root: std::path::PathBuf,
        store: Store,
        runtime: Arc<NodeRuntime>,
        uses: Arc<VaultNodes>,
        _node: Arc<steve_node::LocalNode>,
    }

    /// A Hub in `root` with its local Node on (commands allowed), the secrets channel connected,
    /// and a session working in `root/work`.
    fn hub_at(root: &std::path::Path) -> T {
        let store = Store::open(root.join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                ceiling: steve_node::PolicyCeiling::unlimited(),
                node: NodeId::new("local").unwrap(),
                home: root.join("node"),
                acp: steve_acp::AcpConfig {
                    adapters: Default::default(),
                    node: "node".into(),
                    claude_executable: "/usr/bin/false".into(),
                    steve_home: root.join("steve"),
                    codex_home: root.join("codex"),
                    user_home: root.to_path_buf(),
                    version: "test".into(),
                    idle_release: Duration::from_secs(600),
                    retry_delays: vec![],
                },
                roots: vec![],
                capacity: Capacity {
                    sessions: 0,
                    jobs: 0,
                    execs: 4,
                },
                storage: Default::default(),
                jobs: Default::default(),
            })
            .unwrap(),
        );
        let secrets = NodeSecrets::new();
        node.connect_secrets(secrets.for_node(NodeId::new("local").unwrap()));
        let runtime = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Ends));
        node.connect(Arc::new(Sink(Arc::downgrade(&runtime))), None);
        runtime.start();
        let ws = Workspaces::new(
            store.clone(),
            Some(LocalFace {
                node: node.clone(),
                runtime: runtime.clone(),
            }),
            None,
            vec![],
        );
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?1,'default','claude',?2,'running',1,1)",
                    [SESSION, work.to_str().unwrap()],
                )?;
                Ok(())
            })
            .unwrap();
        T {
            uses: VaultNodes::new(store.clone(), ws, secrets),
            root: root.to_path_buf(),
            store,
            runtime,
            _node: node,
        }
    }

    fn inj(var: &str, how: How) -> Injection {
        Injection {
            var: var.into(),
            label: "令牌".into(),
            how,
        }
    }

    fn use_of(id: &str, command: &str, injections: Vec<Injection>) -> NodeUse {
        NodeUse {
            use_id: id.into(),
            ws: "default".into(),
            session: SESSION.into(),
            cwd: Some("sub".into()),
            command: command.into(),
            injections,
            timeout: Duration::from_secs(10),
        }
    }

    /// Every byte of every file under `dir`.
    fn all_bytes(dir: &std::path::Path) -> Vec<u8> {
        let mut out = vec![];
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(all_bytes(&p));
            } else if let Ok(b) = std::fs::read(&p) {
                out.extend(b);
            }
        }
        out
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[tokio::test]
    async fn a_secret_is_taken_once_by_its_own_node_for_its_own_operation() {
        let all = NodeSecrets::new();
        let (local, far) = (NodeId::new("local").unwrap(), NodeId::new("n_far").unwrap());
        let values = Arc::new(Values::default());
        all.give("op_a", local.clone(), vec!["令牌".into()], values.clone());
        let op = |s: &str| OperationId::new(s).unwrap();
        let code = |r: ApiResult<SecretBytes>| r.unwrap_err().code;
        // another Node, another field, an operation given nothing
        assert_eq!(
            code(all.for_node(far).take(op("op_a"), "令牌".into()).await),
            ErrorCode::Denied
        );
        let mine = all.for_node(local);
        assert_eq!(
            code(mine.take(op("op_a"), "名称".into()).await),
            ErrorCode::Denied
        );
        assert_eq!(
            code(mine.take(op("op_b"), "令牌".into()).await),
            ErrorCode::Denied
        );
        assert_eq!(values.0.load(Ordering::SeqCst), 0, "nothing decrypted yet");
        // its own: once, decrypted then
        let v = mine.take(op("op_a"), "令牌".into()).await.unwrap();
        assert_eq!(v.expose(), TOKEN.as_bytes());
        assert_eq!(values.0.load(Ordering::SeqCst), 1);
        // a replay, and anything after it ended
        assert_eq!(
            code(mine.take(op("op_a"), "令牌".into()).await),
            ErrorCode::Stale
        );
        all.end("op_a");
        assert_eq!(
            code(mine.take(op("op_a"), "令牌".into()).await),
            ErrorCode::Stale
        );
        assert_eq!(values.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_push_grant_is_not_reset_or_resurrected_by_another_wait() {
        let all = NodeSecrets::new();
        let node = NodeId::new("local").unwrap();
        let values = Arc::new(Values::default());
        let give = || {
            all.give_once(
                "op_push",
                node.clone(),
                vec!["token".into()],
                values.clone(),
                || Ok(true),
            )
            .unwrap();
        };
        let mine = all.for_node(node.clone());
        let take = || mine.take(OperationId::new("op_push").unwrap(), "token".into());
        give();
        take().await.unwrap();
        give();
        let refused: Result<(), _> = all.submit_push(
            "op_push",
            node.clone(),
            vec!["token".into()],
            values.clone(),
            || Ok(true),
            || Err(StoreError::Conflict("test submit failure".into())),
        );
        assert!(refused.is_err());
        let error = take().await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Stale);
        assert_eq!(error.safe_detail, "already taken");
        all.end("op_push");
        give();
        let error = take().await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Stale);
        assert_eq!(error.safe_detail, "the operation ended");
        assert_eq!(values.0.load(Ordering::SeqCst), 1);

        all.retire_push("op_retired");
        all.give_once(
            "op_retired",
            node.clone(),
            vec!["token".into()],
            values.clone(),
            || Ok(true),
        )
        .unwrap();
        let error = mine
            .take(OperationId::new("op_retired").unwrap(), "token".into())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Stale);
        assert_eq!(error.safe_detail, "the operation ended");
        assert_eq!(values.0.load(Ordering::SeqCst), 1);

        // A restarted Hub has no in-memory grant or tombstone, and may supply the same
        // still-pending operation after checking its ledger again.
        let restarted = NodeSecrets::new();
        restarted
            .give_once(
                "op_push",
                node.clone(),
                vec!["token".into()],
                values.clone(),
                || Ok(true),
            )
            .unwrap();
        restarted
            .for_node(node)
            .take(OperationId::new("op_push").unwrap(), "token".into())
            .await
            .unwrap();
        assert_eq!(values.0.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_use_runs_once_on_the_sessions_node_masked_and_kept_nowhere() {
        let tmp = tempfile::tempdir().unwrap();
        let t = hub_at(tmp.path());
        assert_eq!(
            t.uses.place("default", SESSION).await.unwrap(),
            Some(NodePlace {
                device: "hub".into(),
                files: true,
            })
        );
        assert_eq!(t.uses.place("other", SESSION).await.unwrap(), None);
        let count = t.root.join("count");
        let values = Arc::new(Values::default());
        let u = use_of(
            "su_1",
            &format!(
                "echo ran >> '{}'; pwd; echo \"t=$TOK\"; printf %s \"$TOK\" | base64; read -r x; echo \"in=$x\"; echo \"f=$KEYF\"; cat \"$KEYF\"; echo; echo err \"$TOK\" >&2",
                count.display()
            ),
            vec![
                inj("TOK", How::Env),
                inj("", How::Stdin),
                inj("KEYF", How::File),
            ],
        );
        let out = t.uses.run(u.clone(), values.clone()).await;
        let sub = t.root.join("work/sub").canonicalize().unwrap();
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        assert_eq!(
            out.stdout,
            format!(
                "{}\nt=[已打码：令牌]\n[已打码：令牌]\nin=[已打码：令牌]\nf=[已打码：临时文件]/KEYF\n[已打码：令牌]\n",
                sub.display()
            )
        );
        assert_eq!(out.stderr, "err [已打码：令牌]\n");
        assert_eq!(
            values.0.load(Ordering::SeqCst),
            1,
            "taken once for its three uses"
        );
        let l = t.runtime.ledger(&op_of("su_1")).unwrap().unwrap();
        assert_eq!(
            (l.subject.as_str(), l.state.as_str()),
            ("vault_use:su_1", "finished")
        );
        // asked again: the answer it had, nothing taken or run again
        assert_eq!(t.uses.run(u, values.clone()).await, out);
        assert_eq!(values.0.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(std::fs::read_to_string(&count).unwrap(), "ran\n");

        // the deadline: as the vault's own run says it
        let mut slow = use_of("su_2", "echo start; sleep 30", vec![inj("TOK", How::Env)]);
        slow.timeout = Duration::from_millis(500);
        let out = t.uses.run(slow, values.clone()).await;
        assert!(out.timed_out && out.exit_code.is_none(), "{out:?}");
        assert_eq!(out.stdout, "start\n");

        // a directory that is not there is the Node's to refuse; nothing runs
        let mut gone = use_of("su_3", "echo ran >> nowhere", vec![inj("TOK", How::Env)]);
        gone.cwd = Some("missing".into());
        let out = t.uses.run(gone, values.clone()).await;
        assert!(out.error.is_some() && out.exit_code.is_none(), "{out:?}");

        // the ledger, the Hub's store and the Node's journal hold no value in any form
        t._node.shutdown().await;
        let kept = all_bytes(&t.root);
        assert!(
            contains(&kept, b"vault_use:su_1"),
            "the ledger was looked at"
        );
        for form in [TOKEN.as_bytes().to_vec(), hex::encode(TOKEN).into_bytes()] {
            assert!(!contains(&kept, &form), "a value on disk");
        }
        let _ = t.store;
    }

    /// A token written into the command itself is never sent: the ledger and the Node's journal
    /// keep the command as it is. A `$VAR` after `password=` is a reference and runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_token_pasted_into_the_command_is_not_sent_or_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let t = hub_at(tmp.path());
        let pasted = "ghp_PastedIntoTheCommand0123456789abcd";
        let values = Arc::new(Values::default());
        let out = t
            .uses
            .run(
                use_of(
                    "su_p",
                    &format!("curl -H 'Authorization: Bearer {pasted}' -H \"X: $TOK\" x"),
                    vec![inj("TOK", How::Env)],
                ),
                values.clone(),
            )
            .await;
        assert_eq!(out.error.as_deref(), Some(PASTED), "{out:?}");
        assert!(t.runtime.ledger(&op_of("su_p")).unwrap().is_none());
        assert_eq!(values.0.load(Ordering::SeqCst), 0);

        let out = t
            .uses
            .run(
                use_of("su_r", "echo \"password=$TOK\"", vec![inj("TOK", How::Env)]),
                values.clone(),
            )
            .await;
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        assert_eq!(out.stdout, "password=[已打码：令牌]\n");

        t._node.shutdown().await;
        let all = all_bytes(&t.root);
        assert!(
            contains(&all, b"vault_use:su_r"),
            "the ledger was looked at"
        );
        let node = all_bytes(&t.root.join("node"));
        assert!(
            contains(&node, b"password=$TOK"),
            "the journal was looked at"
        );
        assert!(!contains(&all, pasted.as_bytes()), "a pasted token on disk");
    }

    /// Runs in a child process (see `a_hub_that_dies_mid_use_never_runs_it_again`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn child_vault_use_crash() {
        let Ok(root) = std::env::var("STEVE_VAULT_USE_CHILD_ROOT") else {
            return;
        };
        let t = hub_at(std::path::Path::new(&root));
        let u = use_of(
            "su_lost",
            &format!(
                "echo ran >> '{root}/runs'; echo $$ > '{root}/pid'; exec sleep 120",
                root = t.root.display()
            ),
            vec![inj("TOK", How::Env)],
        );
        let uses = t.uses.clone();
        tokio::spawn(async move { uses.run(u, Arc::new(Values::default())).await });
        let op = op_of("su_lost");
        let t0 = Instant::now();
        while !(t.root.join("pid").exists()
            && t.runtime
                .ledger(&op)
                .unwrap()
                .is_some_and(|l| l.state != "pending"))
        {
            assert!(t0.elapsed() < Duration::from_secs(20));
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        std::fs::write(t.root.join("ready"), "").unwrap();
        tokio::time::sleep(Duration::from_secs(300)).await;
    }

    /// The Hub (and its local Node) is killed while a use's command runs. After the restart the
    /// use's command is unknown: never sent again, its use answered as lost, and it ran once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hub_that_dies_mid_use_never_runs_it_again() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "wire::node_secrets::tests::child_vault_use_crash",
                "--nocapture",
            ])
            .env("STEVE_VAULT_USE_CHILD_ROOT", tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let t0 = Instant::now();
        while !tmp.path().join("ready").exists() {
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "the child never got ready"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        child.kill().unwrap();
        child.wait().unwrap();

        let t = hub_at(tmp.path());
        let op = op_of("su_lost");
        // once the Hub has reconciled with the Node: unknown, not sent again
        let t0 = Instant::now();
        while t.runtime.ledger(&op).unwrap().unwrap().state != "unknown" {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "the lost command is unknown"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // the use asked again (it is not: the vault fails it at startup) gets the lost answer
        let values = Arc::new(Values::default());
        let out = t
            .uses
            .run(
                use_of("su_lost", "echo again >> runs", vec![inj("TOK", How::Env)]),
                values.clone(),
            )
            .await;
        assert_eq!(out.error.as_deref(), Some(UNKNOWN), "{out:?}");
        assert_eq!(
            values.0.load(Ordering::SeqCst),
            0,
            "no value after a restart"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("runs")).unwrap(),
            "ran\n"
        );
        // what is left of it goes with a recover, never a second run
        let pid: i32 = std::fs::read_to_string(tmp.path().join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(t.runtime.stop_lost(&op).unwrap());
        let t0 = Instant::now();
        while unsafe { libc::kill(-pid, 0) } == 0 {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "the lost command is stopped"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
