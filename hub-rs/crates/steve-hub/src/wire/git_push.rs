//! GitHub's git on the Node that holds an item or a project (N0e-E8), for a Hub whose local Node
//! is on (`STEVE_NODE_MODE=local`): [`steve_github::push::GitNodes`].
//!
//! Reads (whether the branch has commits to offer, where it is, a project's `origin`) are the
//! Node's git facts in the item's workspace (its bound session's, else its directory prepared as
//! the workspace `item:<id>:<dir>`). The push is a [`GitAction::Push`] written to the Node's
//! ledger under `push:<item>` before it is sent, so it is sent at most once: its credential goes
//! as a reference ([`SecretRef`], delivered as an HTTP header) and the Node takes the value once
//! through [`NodeSecrets`], right before git runs. A push the Node refused before running because
//! a ref moved meanwhile is asked again under the next operation (a bounded few times). One whose
//! end is not known (a restart, the deadline, an unknown the ledger stops) is never sent again:
//! [`PushError::Lost`], and GitHub's branch says whether it landed.
//!
//! Every answer is `None` with the local Node off: the Hub's own git runs as before.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use steve_github::push::{GitNodes, NodePush, PushError, branch_ok};
use steve_modules::vault::secret::SecretBytes as VaultBytes;
use steve_modules::vault::{UseValues, VaultResult};
use steve_node_api::*;
use zeroize::Zeroizing;

use super::flow_cmd::{dir_digest, prepare_dir, resource_of};
use super::node_runtime::{LedgerEntry, NodeRuntime};
use super::node_secrets::NodeSecrets;
use super::workspaces::{At, Placement, PlacementError, Subject, Workspaces};

/// The id the push's credential goes by (what the mask shows).
const SECRET_ID: &str = "github";
/// The header git sends it in.
const HEADER: &str = "AUTHORIZATION";
/// Past the Node's own deadline for a push (5 minutes), how long its answer may take.
const ANSWER_WAIT: Duration = Duration::from_secs(6 * 60);
/// How long a push the ledger lost may take to be stopped before it is compared.
const LOST_WAIT: Duration = Duration::from_secs(60);
/// How many times a push refused because a ref moved meanwhile is asked again.
const TRIES: u32 = 3;

pub struct GitPush {
    workspaces: Arc<Workspaces>,
    secrets: Arc<NodeSecrets>,
    /// How long to wait for a push's answer (tests shorten it).
    answer_wait: Duration,
}

impl GitPush {
    pub fn new(workspaces: Arc<Workspaces>, secrets: Arc<NodeSecrets>) -> Arc<GitPush> {
        Arc::new(GitPush {
            workspaces,
            secrets,
            answer_wait: ANSWER_WAIT,
        })
    }
}

/// The header's value for a token, as GitHub takes it over HTTPS.
struct HeaderValue(Zeroizing<Vec<u8>>);

impl HeaderValue {
    fn of(token: &str) -> HeaderValue {
        let pair = Zeroizing::new(format!("x-access-token:{token}"));
        let b64 = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(pair.as_bytes()));
        let mut v = Zeroizing::new(Vec::with_capacity(6 + b64.len()));
        v.extend_from_slice(b"basic ");
        v.extend_from_slice(b64.as_bytes());
        HeaderValue(v)
    }
}

impl UseValues for HeaderValue {
    fn value(&self, _label: &str) -> VaultResult<VaultBytes> {
        Ok(VaultBytes::new(self.0.to_vec()))
    }
}

/// The operation of try `n` of the push for `key` (the decision).
fn op_of(key: &str, n: u32) -> String {
    match n {
        0 => derived_id("op_push", key),
        n => derived_id("op_push", &format!("{key}/{n}")),
    }
}

/// Stored text (the item's timeline, its fail card): zh-CN whoever's request it came in.
fn node_error(e: impl std::fmt::Display) -> PushError {
    PushError::Node(steve_i18n::stored(|| e.to_string()))
}

/// What the Node said, as the Hub's own push says it. `now`: where the branch is now, for a
/// branch that moved.
fn mapped(e: &ApiError, p: &NodePush<'_>, now: Option<String>) -> PushError {
    use steve_node::git::{BRANCH_MOVED, HOST_FORBIDDEN, NO_BRANCH, WRONG_REMOTE};
    match (e.code, e.safe_detail.as_str()) {
        (ErrorCode::Offline, _) => node_error(PlacementError::Offline {
            device: String::new(),
        }),
        (ErrorCode::Denied, WRONG_REMOTE) => PushError::WrongRemote {
            expected: format!("github.com/{}", p.full_name),
        },
        (ErrorCode::Denied, HOST_FORBIDDEN) => PushError::HostForbidden(
            steve_modules::net::check_url_with(true, p.url)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| HOST_FORBIDDEN.into()),
        ),
        (ErrorCode::Stale, NO_BRANCH) => PushError::NoBranch(p.branch.into()),
        (ErrorCode::Stale, BRANCH_MOVED) => PushError::Moved {
            branch: p.branch.into(),
            now: now.unwrap_or_default().chars().take(7).collect(),
        },
        _ => PushError::Git(e.safe_detail.chars().take(300).collect()),
    }
}

/// Refused before it ran because something besides the branch moved: asked again.
fn moved_meanwhile(l: &LedgerEntry) -> bool {
    l.state == "rejected"
        && l.refusal.as_ref().is_some_and(|e| {
            e.code == ErrorCode::Stale
                && e.safe_detail != steve_node::git::BRANCH_MOVED
                && e.safe_detail != steve_node::git::NO_BRANCH
        })
}

impl GitPush {
    /// The local roots made current: a directory made a moment ago may not be among them yet.
    async fn refresh(&self) -> Result<(), PushError> {
        let w = self.workspaces.clone();
        tokio::task::spawn_blocking(move || w.refresh_roots())
            .await
            .map_err(node_error)?
            .map_err(node_error)
    }

    /// The Node and place of `what` (the roots refreshed before); `None`: the local Node is off.
    fn placed(&self, what: Subject<'_>) -> Option<Result<Placement, PushError>> {
        match self.workspaces.placement_of(what) {
            Err(PlacementError::LocalOff) => None,
            Err(e) => Some(Err(node_error(e))),
            Ok(p) => Some(Ok(p)),
        }
    }

    /// The workspace of `p`: a bound session's, else `resource` at its directory.
    async fn workspace(
        p: &Placement,
        resource: impl FnOnce(&RootPath) -> String,
    ) -> Result<WorkspaceRef, PushError> {
        match &p.at {
            At::Workspace(w) => Ok(w.clone()),
            At::Root(dir) => prepare_dir(&p.node, resource(dir), dir.clone())
                .await
                .map_err(node_error),
        }
    }

    async fn item(&self, item: &str) -> Option<Result<(Placement, WorkspaceRef), PushError>> {
        let refreshed = self.refresh().await;
        let p = match self.placed(Subject::Item(item))? {
            Ok(p) => p,
            Err(e) => return Some(Err(e)),
        };
        if let Err(e) = refreshed {
            return Some(Err(e));
        }
        let w = Self::workspace(&p, |dir| format!("item:{item}:{}", dir_digest(dir))).await;
        Some(w.map(|w| (p, w)))
    }

    async fn facts(
        node: &Arc<dyn Node>,
        w: &WorkspaceRef,
        refs: Vec<String>,
        reads: Vec<GitRead>,
    ) -> Result<GitFacts, ApiError> {
        node.git_facts(GitQuery {
            workspace: w.clone(),
            refs,
            ranges: vec![],
            fetch: false,
            reads,
            // nothing here reads `dirty`, and a full status is the slow part
            heads_only: true,
        })
        .await
    }

    /// Where the branch is now (`None`: gone, or the Node cannot say).
    async fn branch_now(node: &Arc<dyn Node>, w: &WorkspaceRef, branch: &str) -> Option<String> {
        let f = Self::facts(node, w, vec![format!("refs/heads/{branch}")], vec![])
            .await
            .ok()?;
        f.refs.into_iter().next().and_then(|(_, s)| s)
    }

    /// Waits for `op` to settle.
    async fn settled(&self, runtime: &NodeRuntime, op: &str) -> Option<LedgerEntry> {
        let until = Instant::now() + self.answer_wait;
        loop {
            match runtime.ledger(op) {
                Ok(Some(l)) if l.is_settled() => return Some(l),
                Ok(Some(_)) => {}
                Ok(None) => return None,
                // the store is there again later; the push is in the ledger either way
                Err(e) => tracing::warn!("push {op}: {e}"),
            }
            if Instant::now() >= until {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A push the ledger lost: stopped (asked once) and given a moment to be, never sent again.
    async fn stop(&self, runtime: &NodeRuntime, op: &str) {
        match runtime.stop_lost(op) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::warn!("push {op}: {e}");
                return;
            }
        }
        let until = Instant::now() + LOST_WAIT;
        while Instant::now() < until {
            match runtime.ledger(op) {
                Ok(Some(l)) if l.state == "unknown" => {}
                _ => return,
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn push_on(
        &self,
        placed: &Placement,
        w: &WorkspaceRef,
        p: &NodePush<'_>,
    ) -> Result<String, PushError> {
        let runtime = &placed.runtime;
        let subject = format!("push:{}", p.item);
        for n in 0..TRIES {
            let op = op_of(p.key, n);
            let entry = runtime.ledger(&op).map_err(node_error)?;
            let entry = match entry {
                Some(l) if moved_meanwhile(&l) => continue,
                Some(l) => l,
                None => {
                    // the branch where it was approved, and the facts the Node checks again
                    let f = Self::facts(
                        &placed.node,
                        w,
                        vec![format!("refs/heads/{}", p.branch)],
                        vec![],
                    )
                    .await
                    .map_err(|e| mapped(&e, p, None))?;
                    let local = f.refs.into_iter().next().and_then(|(_, s)| s);
                    match local {
                        None => return Err(PushError::NoBranch(p.branch.into())),
                        Some(s) if s != p.expect => {
                            return Err(PushError::Moved {
                                branch: p.branch.into(),
                                now: s.chars().take(7).collect(),
                            });
                        }
                        Some(_) => {}
                    }
                    let apply = GitApply {
                        workspace: w.clone(),
                        expected: f.revision,
                        expected_head: f.head,
                        action: GitAction::Push {
                            remote: "origin".into(),
                            branch: p.branch.into(),
                            expected_remote: None,
                            url: p.url.into(),
                            origin_must_be: Some(p.full_name.into()),
                            expected_local: p.expect.into(),
                            auth: Some(SecretRef {
                                id: SECRET_ID.into(),
                                how: SecretDelivery::Header {
                                    name: HEADER.into(),
                                },
                            }),
                        },
                    };
                    // given before it can be sent
                    self.give(&op, placed, p);
                    if let Err(e) = runtime.submit(&op, &subject, "git_apply", apply) {
                        self.secrets.end(&op);
                        return Err(node_error(e));
                    }
                    self.wait(placed, &op).await?
                }
            };
            if !entry.is_settled() {
                // written before a restart and not answered yet: what it may take is given again
                self.give(&op, placed, p);
                let l = self.wait(placed, &op).await?;
                return self.result(placed, w, &l, p).await;
            }
            if moved_meanwhile(&entry) {
                continue;
            }
            return self.result(placed, w, &entry, p).await;
        }
        Err(PushError::Git("仓库的引用一直在变，没有推送".into()))
    }

    fn give(&self, op: &str, placed: &Placement, p: &NodePush<'_>) {
        self.secrets.give(
            op,
            placed.node_id.clone(),
            vec![SECRET_ID.into()],
            Arc::new(HeaderValue::of(p.token)),
        );
    }

    /// The settled entry of `op`; its secret is over then. Not settled within the wait (still in
    /// the outbox, or still running on the Node, whose deadline starts when it takes it): it may
    /// still land, so it is [`PushError::Running`], never lost (nothing compares it with GitHub
    /// yet); asked again, the ledger's entry is waited for again.
    async fn wait(&self, placed: &Placement, op: &str) -> Result<LedgerEntry, PushError> {
        let l = self.settled(&placed.runtime, op).await;
        self.secrets.end(op);
        match l {
            Some(l) => Ok(l),
            None if matches!(placed.runtime.ledger(op), Ok(None)) => Err(PushError::Lost),
            None => Err(PushError::Running),
        }
    }

    async fn result(
        &self,
        placed: &Placement,
        w: &WorkspaceRef,
        l: &LedgerEntry,
        p: &NodePush<'_>,
    ) -> Result<String, PushError> {
        let error = match (
            l.state.as_str(),
            &l.refusal,
            l.receipt.as_ref().map(|r| &r.state),
        ) {
            ("rejected", Some(e), _) => e.clone(),
            ("rejected", None, _) => ApiError::new(ErrorCode::Internal, "refused"),
            ("unknown", ..) => {
                self.stop(&placed.runtime, &l.operation).await;
                return Err(PushError::Lost);
            }
            (
                _,
                _,
                Some(Observed::Finished {
                    outcome: Outcome::Succeeded,
                    ..
                }),
            ) => return Ok(p.expect.into()),
            (
                _,
                _,
                Some(Observed::Finished {
                    outcome: Outcome::Failed,
                    error: Some(e),
                    ..
                }),
            ) => e.clone(),
            // timed out, interrupted: it may have landed
            _ => return Err(PushError::Lost),
        };
        let now = if error.safe_detail == steve_node::git::BRANCH_MOVED {
            Self::branch_now(&placed.node, w, p.branch).await
        } else {
            None
        };
        Err(mapped(&error, p, now))
    }
}

#[async_trait]
impl GitNodes for GitPush {
    async fn ahead(&self, item: &str, base: &str, branch: &str) -> Option<Result<bool, PushError>> {
        for b in [branch, base] {
            if !branch_ok(b) {
                return Some(Err(PushError::BadBranch(b.into())));
            }
        }
        let (placed, w) = match self.item(item).await? {
            Ok(x) => x,
            Err(e) => return Some(Err(e)),
        };
        let mine = format!("refs/heads/{branch}");
        let tracking = format!("refs/remotes/origin/{base}");
        let r = async {
            let f = Self::facts(
                &placed.node,
                &w,
                vec![tracking.clone(), mine.clone()],
                vec![],
            )
            .await
            .map_err(|e| PushError::Git(e.safe_detail))?;
            if f.refs.get(1).and_then(|(_, s)| s.as_ref()).is_none() {
                return Err(PushError::Git(format!("没有分支 {branch}")));
            }
            let from = match f.refs.first().and_then(|(_, s)| s.as_ref()) {
                Some(_) => tracking,
                None => format!("refs/heads/{base}"),
            };
            let f = Self::facts(
                &placed.node,
                &w,
                vec![],
                vec![GitRead::IsAncestor {
                    ancestor: mine,
                    of: from,
                }],
            )
            .await
            .map_err(|e| PushError::Git(e.safe_detail))?;
            match f.reads.first().map(|r| &r.answer) {
                // ahead exactly when the branch is not in the base already
                Some(GitAnswer::IsAncestor { yes }) => Ok(!yes),
                _ => Err(PushError::Git("is-ancestor".into())),
            }
        };
        Some(r.await)
    }

    async fn head_of(&self, item: &str, branch: &str) -> Option<Result<String, PushError>> {
        if !branch_ok(branch) {
            return Some(Err(PushError::BadBranch(branch.into())));
        }
        let (placed, w) = match self.item(item).await? {
            Ok(x) => x,
            Err(e) => return Some(Err(e)),
        };
        Some(
            Self::branch_now(&placed.node, &w, branch)
                .await
                .ok_or_else(|| PushError::NoBranch(branch.into())),
        )
    }

    async fn push(&self, p: &NodePush<'_>) -> Option<Result<String, PushError>> {
        if !branch_ok(p.branch) {
            return Some(Err(PushError::BadBranch(p.branch.into())));
        }
        // the test guard, before anything is sent (the Node checks it again)
        if let Err(e) = steve_modules::net::check_url(p.url)
            && !p.url.starts_with('/')
        {
            return Some(Err(PushError::HostForbidden(e.to_string())));
        }
        let (placed, w) = match self.item(p.item).await? {
            Ok(x) => x,
            Err(e) => return Some(Err(e)),
        };
        Some(self.push_on(&placed, &w, p).await)
    }

    async fn origins(&self, projects: &[String]) -> Option<Vec<Option<String>>> {
        use futures::StreamExt as _;
        // once for the whole list
        let refreshed = self.refresh().await.is_ok();
        let asks: Vec<_> = projects.iter().map(|p| self.origin(p, refreshed)).collect();
        let found: Vec<Option<Option<String>>> =
            futures::stream::iter(asks).buffered(4).collect().await;
        found.into_iter().collect()
    }
}

impl GitPush {
    /// `owner/name` of `project`'s `origin` on its Node; `None`: the local Node is off.
    async fn origin(&self, project: &str, refreshed: bool) -> Option<Option<String>> {
        let placed = match self.placed(Subject::Project(project))? {
            Ok(p) if refreshed => p,
            _ => return Some(None),
        };
        let Ok(w) = Self::workspace(&placed, |dir| resource_of(project, dir)).await else {
            return Some(None);
        };
        let f = Self::facts(
            &placed.node,
            &w,
            vec![],
            vec![GitRead::RemoteUrl {
                remote: "origin".into(),
            }],
        )
        .await;
        let Ok(f) = f else {
            return Some(None);
        };
        Some(match f.reads.into_iter().next().map(|r| r.answer) {
            Some(GitAnswer::RemoteUrl { url: Some(u) }) => github_repo(&u),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use steve_store::{Store, StoreError};

    use super::super::node_runtime::Sink;
    use super::super::node_tools::tests::Ends;
    use super::super::workspaces::LocalFace;
    use super::*;

    const BRANCH: &str = "steve/st-1";
    const ITEM: &str = "ST-1";
    const TOKEN: &str = "ghs-HubT0ken-4417";

    struct T {
        root: PathBuf,
        _tmp: Option<tempfile::TempDir>,
        store: Store,
        node: Arc<steve_node::LocalNode>,
        runtime: Arc<NodeRuntime>,
        git: Arc<GitPush>,
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn hub() -> T {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = hub_at(tmp.path());
        t._tmp = Some(tmp);
        t
    }

    /// A Hub in `root` with its local Node on, and (made the first time) the item `ST-1` of a
    /// project in `root/work`, whose origin is github.com/o/r and whose branch is one commit past
    /// `main`; pushes go to `root/remotes/o/r.git`.
    fn hub_at(root: &Path) -> T {
        let work = root.join("work");
        let fresh = !work.exists();
        if fresh {
            std::fs::create_dir_all(&work).unwrap();
            sh(&work, &["init", "-q", "-b", "main"]);
            std::fs::write(work.join("a"), "a").unwrap();
            sh(&work, &["add", "."]);
            sh(&work, &["commit", "-qm", "a"]);
            sh(
                &work,
                &["remote", "add", "origin", "git@github.com:o/r.git"],
            );
            let bare = root.join("remotes/o/r.git");
            std::fs::create_dir_all(&bare).unwrap();
            sh(&bare, &["init", "-q", "--bare", "-b", "main"]);
            sh(&work, &["push", "-q", bare.to_str().unwrap(), "main:main"]);
            sh(&work, &["checkout", "-qb", BRANCH]);
            std::fs::write(work.join("b"), "b").unwrap();
            sh(&work, &["add", "."]);
            sh(&work, &["commit", "-qm", "b"]);
        }
        let store = Store::open(root.join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
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
        if fresh {
            let w = work.to_str().unwrap();
            store
                .transaction::<_, StoreError>(|tx| {
                    tx.execute(
                        "INSERT INTO projects(id,workspace_id,name,path,kind,device_id,created,updated) VALUES ('p1','default','p1',?1,'git','hub',1,1)",
                        [w],
                    )?;
                    tx.execute(
                        "INSERT INTO items (id, workspace_id, no, title, status, stage, cwd, branch, project_id, created, updated)
                         VALUES (?1, 'default', 1, 't', 'wait', 'x', ?2, ?3, 'p1', 1, 1)",
                        [ITEM, w, BRANCH],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        T {
            git: GitPush::new(ws, secrets),
            root: root.to_path_buf(),
            _tmp: None,
            store,
            node,
            runtime,
        }
    }

    impl T {
        fn url(&self) -> String {
            self.root.join("remotes/o/r.git").display().to_string()
        }
        fn tip(&self) -> String {
            sh(&self.root.join("work"), &["rev-parse", BRANCH])
        }
        fn remote(&self) -> Option<String> {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(self.root.join("remotes/o/r.git"))
                .args([
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{BRANCH}"),
                ])
                .output()
                .unwrap();
            o.status
                .success()
                .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
        }
        fn pushes(&self) -> i64 {
            self.store
                .reader(|c| {
                    c.query_row(
                        "SELECT COUNT(*) FROM node_commands WHERE method='git_apply'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(StoreError::from)
                })
                .unwrap()
        }
    }

    fn spec<'a>(key: &'a str, url: &'a str, expect: &'a str) -> NodePush<'a> {
        NodePush {
            item: ITEM,
            key,
            branch: BRANCH,
            full_name: "o/r",
            url,
            token: TOKEN,
            expect,
        }
    }

    fn bytes_under(dir: &Path) -> Vec<u8> {
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(bytes_under(&p));
                } else if let Ok(b) = std::fs::read(&p) {
                    out.extend(b);
                }
            }
        }
        out
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_branch_is_read_and_pushed_on_the_items_node_once() {
        let t = hub();
        assert_eq!(t.git.ahead(ITEM, "main", BRANCH).await.unwrap(), Ok(true));
        assert_eq!(t.git.ahead(ITEM, BRANCH, BRANCH).await.unwrap(), Ok(false));
        let tip = t.tip();
        assert_eq!(
            t.git.head_of(ITEM, BRANCH).await.unwrap().as_deref(),
            Ok(tip.as_str())
        );
        assert_eq!(
            t.git.head_of(ITEM, "steve/none").await.unwrap(),
            Err(PushError::NoBranch("steve/none".into()))
        );
        assert_eq!(
            t.git.origins(&["p1".into(), "p2".into()]).await,
            Some(vec![Some("o/r".into()), None])
        );

        let url = t.url();
        assert_eq!(
            t.git.push(&spec("dec_1", &url, &tip)).await.unwrap(),
            Ok(tip.clone())
        );
        assert_eq!(t.remote().as_deref(), Some(tip.as_str()));
        let l = t.runtime.ledger(&op_of("dec_1", 0)).unwrap().unwrap();
        assert_eq!(
            (l.subject.as_str(), l.method.as_str(), l.state.as_str()),
            ("push:ST-1", "git_apply", "finished")
        );
        // asked again: its answer, nothing sent again
        assert_eq!(
            t.git.push(&spec("dec_1", &url, &tip)).await.unwrap(),
            Ok(tip.clone())
        );
        assert_eq!(t.pushes(), 1);
        // the credential is in no store, journal or config
        t.node.shutdown().await;
        let header = HeaderValue::of(TOKEN);
        let mut kept = bytes_under(&t.root.join("node"));
        kept.extend(std::fs::read(t.root.join("hub.sqlite3")).unwrap());
        kept.extend(std::fs::read(t.root.join("hub.sqlite3-wal")).unwrap_or_default());
        kept.extend(bytes_under(&t.root.join("work/.git")));
        assert!(!contains(&kept, TOKEN.as_bytes()));
        assert!(!contains(&kept, &header.0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_push_is_refused_as_the_hubs_own_was() {
        let t = hub();
        let url = t.url();
        let tip = t.tip();
        // the workspace's origin is another repository
        let mut s = spec("dec_w", &url, &tip);
        s.full_name = "o/other";
        assert_eq!(
            t.git.push(&s).await.unwrap(),
            Err(PushError::WrongRemote {
                expected: "github.com/o/other".into()
            })
        );
        // the branch moved after the approval
        let old = "0".repeat(40);
        let r = t.git.push(&spec("dec_m", &url, &old)).await.unwrap();
        assert_eq!(
            r,
            Err(PushError::Moved {
                branch: BRANCH.into(),
                now: tip.chars().take(7).collect()
            })
        );
        // a branch name Steve does not push
        let mut s = spec("dec_b", &url, &tip);
        s.branch = "a b";
        assert!(matches!(
            t.git.push(&s).await.unwrap(),
            Err(PushError::BadBranch(_))
        ));
        assert_eq!(t.remote(), None, "nothing pushed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_test_guard_refuses_before_anything_is_sent() {
        if !steve_modules::net::forbidding_child(
            "wire::git_push::tests::the_test_guard_refuses_before_anything_is_sent",
        ) {
            return;
        }
        let t = hub();
        let tip = t.tip();
        let r = t
            .git
            .push(&spec("dec_f", "https://github.com/o/r.git", &tip))
            .await
            .unwrap();
        assert!(matches!(r, Err(PushError::HostForbidden(_))), "{r:?}");
        assert_eq!(t.pushes(), 0, "nothing written to the ledger");
    }

    /// A server on loopback that takes git's requests and never answers; how many came.
    fn hanging() -> (u16, Arc<Mutex<usize>>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let n = Arc::new(Mutex::new(0));
        let seen = n.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                *seen.lock().unwrap() += 1;
                std::thread::spawn(move || {
                    let mut s = s;
                    let mut b = [0u8; 4096];
                    while matches!(s.read(&mut b), Ok(n) if n > 0) {}
                });
            }
        });
        (port, n)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hanging_push_is_lost_at_its_deadline() {
        let t = hub();
        t.node.set_push_timeout(Duration::from_secs(2));
        let (port, seen) = hanging();
        let url = format!("http://127.0.0.1:{port}/o/r.git");
        let tip = t.tip();
        let t0 = Instant::now();
        assert_eq!(
            t.git.push(&spec("dec_h", &url, &tip)).await.unwrap(),
            Err(PushError::Lost)
        );
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
        assert_eq!(*seen.lock().unwrap(), 1);
        // asked again (the next run of the effect): not pushed again
        assert_eq!(
            t.git.push(&spec("dec_h", &url, &tip)).await.unwrap(),
            Err(PushError::Lost)
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(*seen.lock().unwrap(), 1);
        assert_eq!(t.pushes(), 1);
    }

    /// The Node took the push late (it sat in the outbox), so its own deadline ends after the
    /// Hub's wait: still running then, it is not lost (nothing compares it with GitHub while it
    /// may land). Asked again, its end is waited for again, and it is never sent twice.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_push_still_running_when_the_wait_ends_is_not_lost() {
        let t = hub();
        t.node.set_push_timeout(Duration::from_secs(3));
        let (port, seen) = hanging();
        let url = format!("http://127.0.0.1:{port}/o/r.git");
        let tip = t.tip();
        let short = Arc::new(GitPush {
            workspaces: t.git.workspaces.clone(),
            secrets: t.git.secrets.clone(),
            answer_wait: Duration::from_millis(500),
        });
        assert_eq!(
            short.push(&spec("dec_r", &url, &tip)).await.unwrap(),
            Err(PushError::Running)
        );
        let l = t.runtime.ledger(&op_of("dec_r", 0)).unwrap().unwrap();
        assert!(!l.is_settled(), "{}", l.state);
        // the next run: the Node's deadline ends it, and only then is it lost
        assert_eq!(
            t.git.push(&spec("dec_r", &url, &tip)).await.unwrap(),
            Err(PushError::Lost)
        );
        let l = t.runtime.ledger(&op_of("dec_r", 0)).unwrap().unwrap();
        assert_eq!(l.state, "finished");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(*seen.lock().unwrap(), 1);
        assert_eq!(t.pushes(), 1);
    }

    /// Runs in a child process (see the test below): a Hub whose push hangs when it is killed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn child_push_crash() {
        let (Ok(root), Ok(port)) = (
            std::env::var("STEVE_GIT_PUSH_CHILD_ROOT"),
            std::env::var("STEVE_GIT_PUSH_CHILD_PORT"),
        ) else {
            return;
        };
        let t = hub_at(Path::new(&root));
        let url = format!("http://127.0.0.1:{port}/o/r.git");
        let tip = t.tip();
        let git = t.git.clone();
        tokio::spawn(async move { git.push(&spec("dec_c", &url, &tip)).await });
        let op = op_of("dec_c", 0);
        let t0 = Instant::now();
        while !t
            .runtime
            .ledger(&op)
            .unwrap()
            .is_some_and(|l| l.state == "active" || l.state == "accepted")
        {
            assert!(t0.elapsed() < Duration::from_secs(20));
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        std::fs::write(t.root.join("ready"), "").unwrap();
        tokio::time::sleep(Duration::from_secs(120)).await;
    }

    /// A Hub killed while its push hangs (in a child process): its root, and how many pushes
    /// reached the remote.
    async fn crashed_push() -> (tempfile::TempDir, u16, Arc<Mutex<usize>>) {
        let tmp = tempfile::tempdir().unwrap();
        let (port, seen) = hanging();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "wire::git_push::tests::child_push_crash",
                "--nocapture",
            ])
            .env("STEVE_GIT_PUSH_CHILD_ROOT", tmp.path())
            .env("STEVE_GIT_PUSH_CHILD_PORT", port.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let t0 = Instant::now();
        while !(tmp.path().join("ready").exists() && *seen.lock().unwrap() > 0) {
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "the child never pushed"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        child.kill().unwrap();
        child.wait().unwrap();
        (tmp, port, seen)
    }

    /// The Hub (and its local Node with it) is killed while a push hangs. After the restart it is
    /// unknown: asked again it is stopped, never sent again, and lost (GitHub's branch decides).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_push_lost_in_a_crash_is_never_pushed_again() {
        let (tmp, port, seen) = crashed_push().await;
        let t = hub_at(tmp.path());
        let url = format!("http://127.0.0.1:{port}/o/r.git");
        let tip = t.tip();
        assert_eq!(
            t.git.push(&spec("dec_c", &url, &tip)).await.unwrap(),
            Err(PushError::Lost)
        );
        let op = op_of("dec_c", 0);
        let t0 = Instant::now();
        while t.runtime.ledger(&op).unwrap().unwrap().state != "finished" {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "the lost push is stopped"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(*seen.lock().unwrap(), 1, "pushed once");
        assert_eq!(t.pushes(), 1);
    }

    /// Lost in a crash and never asked about again (what it was for does not run again): past
    /// its deadline the ledger's sweep stops it all the same, and it is not sent again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_push_lost_in_a_crash_is_stopped_past_its_deadline() {
        let (tmp, port, seen) = crashed_push().await;
        let t = hub_at(tmp.path());
        let op = op_of("dec_c", 0);
        let t0 = Instant::now();
        while t.runtime.ledger(&op).unwrap().unwrap().state != "unknown" {
            assert!(t0.elapsed() < Duration::from_secs(30), "reconciled");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // not before its deadline
        assert_eq!(t.runtime.recover_lost_execs(steve_store::now()).unwrap(), 0);
        let later = steve_store::now() + super::super::node_runtime::PUSH_DEADLINE;
        assert_eq!(t.runtime.recover_lost_execs(later).unwrap(), 1);
        while t.runtime.ledger(&op).unwrap().unwrap().state != "finished" {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "the lost push is stopped"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // asked after all: lost, not sent again
        let url = format!("http://127.0.0.1:{port}/o/r.git");
        let tip = t.tip();
        assert_eq!(
            t.git.push(&spec("dec_c", &url, &tip)).await.unwrap(),
            Err(PushError::Lost)
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(*seen.lock().unwrap(), 1, "pushed once");
        assert_eq!(t.pushes(), 1);
    }
}
