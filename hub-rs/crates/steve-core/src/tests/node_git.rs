//! An item whose git is on a Node (N0e-E11b), against a real local Node: its delivery is frozen and
//! checked through the Node off the core's lock, an unreachable Node leaves the closing as it is,
//! and the rebase before a release is applied there only on the facts it was decided on.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_node_api::*;

use crate::git::tests::{branch_with, g, repo};
use crate::node_git::{Applied, Ask, NodeGit};
use crate::patch;
use crate::repo::{Release, WS};
use crate::testkit::Kit;

/// The core's port onto a real `LocalNode` whose one root is the kit's home: each item's `cwd`
/// is prepared as an existing directory there the first time it is asked about.
struct OnNode {
    rt: tokio::runtime::Runtime,
    node: steve_node::LocalNode,
    _home: tempfile::TempDir,
    root: PathBuf,
    store: crate::repo::Db,
    items: Mutex<HashSet<String>>,
    workspaces: Mutex<HashMap<String, WorkspaceRef>>,
    offline: AtomicBool,
    /// Runs once, right before the next apply (something moves meanwhile).
    before_apply: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    applies: Mutex<Vec<String>>,
}

fn command<T>(op: &str, body: T) -> Command<T> {
    let now = crate::util::now();
    Command {
        id: OperationId::new(op).unwrap(),
        issued_at: Timestamp(now),
        accept_before: Timestamp(now + 60_000),
        grant: GrantId::new("grant").unwrap(),
        body,
    }
}

async fn finished(n: &steve_node::LocalNode, op: &OperationId) -> Observed {
    for _ in 0..1500 {
        let r = n.operation(op.clone()).await.unwrap();
        if r.state.is_finished() || matches!(r.state, Observed::Unknown { .. }) {
            return r.state;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{op} never finished")
}

impl OnNode {
    fn new(k: &Kit) -> Arc<OnNode> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let root = k.home().to_path_buf();
        let cfg = steve_node::NodeConfig {
            node: NodeId::new("local").unwrap(),
            home: home.path().join("node"),
            acp: steve_acp::AcpConfig {
                adapters: HashMap::new(),
                node: "node".into(),
                claude_executable: "claude".into(),
                steve_home: home.path().join("steve"),
                codex_home: home.path().join("codex"),
                user_home: home.path().into(),
                version: "test".into(),
                idle_release: Duration::from_secs(600),
                retry_delays: vec![],
            },
            roots: vec![steve_node::Root {
                id: RootId::new("home").unwrap(),
                path: root.clone(),
                display: "~".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 0,
                jobs: 0,
                execs: 0,
            },
            storage: steve_node::Storage::default(),
        };
        let node = {
            let _in = rt.enter();
            steve_node::LocalNode::open(cfg).unwrap()
        };
        let me = Arc::new(OnNode {
            rt,
            node,
            _home: home,
            root,
            store: k.core.store().clone(),
            items: Default::default(),
            workspaces: Default::default(),
            offline: AtomicBool::new(false),
            before_apply: Mutex::new(None),
            applies: Mutex::new(vec![]),
        });
        k.core.set_node_git(me.clone());
        me
    }

    fn put(&self, item: &str) {
        self.items.lock().unwrap().insert(item.into());
    }

    /// The item's workspace: its `cwd` under the root, prepared once.
    async fn workspace(&self, item: &str) -> ApiResult<WorkspaceRef> {
        if let Some(w) = self.workspaces.lock().unwrap().get(item) {
            return Ok(w.clone());
        }
        let cwd = self.store.item(item).unwrap().unwrap().cwd.unwrap();
        let rel = Path::new(&cwd)
            .strip_prefix(&self.root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let op = format!("prep-{item}");
        let r = self
            .node
            .prepare_workspace(command(
                &op,
                Prepare {
                    resource: format!("item:{item}"),
                    kind: PrepareKind::Existing {
                        dir: RootPath {
                            root: RootId::new("home").unwrap(),
                            rel: RelPath::new(rel).unwrap(),
                        },
                    },
                },
            ))
            .await?;
        let Observed::Finished {
            result: Some(OperationResult::Workspace(v)),
            ..
        } = finished(&self.node, &r.operation).await
        else {
            panic!("not prepared")
        };
        self.workspaces
            .lock()
            .unwrap()
            .insert(item.into(), v.workspace.clone());
        Ok(v.workspace)
    }

    fn offline(&self) -> ApiResult<()> {
        match self.offline.load(Ordering::SeqCst) {
            true => Err(ApiError::new(ErrorCode::Offline, "设备离线")),
            false => Ok(()),
        }
    }
}

#[async_trait]
impl NodeGit for OnNode {
    fn on_node(&self, item: &str) -> bool {
        self.items.lock().unwrap().contains(item)
    }

    async fn facts(&self, item: &str, q: Ask) -> ApiResult<GitFacts> {
        self.offline()?;
        let w = self.workspace(item).await?;
        let node = self.node.clone();
        self.rt
            .spawn(async move {
                node.git_facts(GitQuery {
                    workspace: w,
                    refs: q.refs,
                    ranges: q.ranges,
                    fetch: q.fetch,
                    reads: q.reads,
                    heads_only: false,
                })
                .await
            })
            .await
            .unwrap()
    }

    async fn apply(
        &self,
        item: &str,
        operation: &str,
        _subject: &str,
        on: &GitFacts,
        action: GitAction,
    ) -> Applied {
        if let Some(f) = self.before_apply.lock().unwrap().take() {
            f();
        }
        if let Err(e) = self.offline() {
            return Applied::Failed(e);
        }
        self.applies.lock().unwrap().push(operation.into());
        let w = match self.workspace(item).await {
            Ok(w) => w,
            Err(e) => return Applied::Failed(e),
        };
        let node = self.node.clone();
        let c = command(
            operation,
            GitApply {
                workspace: w,
                expected: on.revision.clone(),
                expected_head: on.head.clone(),
                action,
            },
        );
        self.rt
            .spawn(async move {
                let r = match node.git_apply(c).await {
                    Ok(r) => r,
                    Err(e) => return Applied::Failed(e),
                };
                match finished(&node, &r.operation).await {
                    Observed::Finished {
                        result: Some(OperationResult::Git(f)),
                        ..
                    } => Applied::Done(f),
                    Observed::Finished { error: Some(e), .. } => Applied::Failed(e),
                    other => Applied::Unknown(format!("{other:?}")),
                }
            })
            .await
            .unwrap()
    }
}

/// An item delivered from the worktree of its branch, its git on the Node.
fn delivered_on_node(k: &Kit, port: &OnNode) -> (String, PathBuf) {
    let r = repo(k.home());
    let i = k.item("run", "交付与验收", None);
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &i, "b.txt", "b\n");
    k.db()
        .item_patch(
            &i,
            patch![cwd: wt.to_string_lossy().to_string(), branch: name.as_str()],
        )
        .unwrap();
    k.session(&i, "idle");
    port.put(&i);
    k.core
        .tx()
        .lc_request_complete(
            &i,
            "dev",
            crate::cycle::Policy::Human,
            false,
            "交付，等你验收",
            None,
            vec![],
        )
        .unwrap();
    (i, wt)
}

fn closing(k: &Kit, i: &str) -> crate::cycle::Closing {
    crate::cycle::closing_of(&k.db().item(i).unwrap().unwrap()).unwrap()
}

#[test]
fn a_delivery_on_a_node_is_frozen_and_checked_there() {
    let k = Kit::new();
    let port = OnNode::new(&k);
    let (i, wt) = delivered_on_node(&k, &port);
    /* frozen once the Node's facts came: what the Hub's own git says of the same worktree */
    assert!(k.wait(|| !closing(&k, &i).print_pending));
    let frozen = closing(&k, &i).fingerprint.unwrap();
    assert_eq!(
        Some(&frozen),
        crate::cycle::fingerprint(wt.to_str()).as_ref()
    );
    assert_eq!(k.phase(&i), "closing");

    /* the result moves: the gate learns it from the Node and says what moved */
    std::fs::write(wt.join("c.txt"), "c\n").unwrap();
    let now = crate::cycle::fingerprint(wt.to_str()).unwrap();
    let words = crate::upstream::describe_change(&wt, &frozen, &now);
    assert!(!k.core.tx().lc_try_close(&i).unwrap());
    assert!(k.wait(|| k.phase(&i) == "active"));
    let tl = k.timeline_texts(&i);
    assert!(
        tl.iter()
            .any(|t| t.contains(&format!("冻结之后结果又变了：{words}"))),
        "{tl:?}"
    );
    assert_eq!(words, "改了 c.txt");
}

#[test]
fn an_unreachable_node_during_the_check_is_unknown_not_changed() {
    let k = Kit::new();
    let port = OnNode::new(&k);
    let (i, wt) = delivered_on_node(&k, &port);
    assert!(k.wait(|| !closing(&k, &i).print_pending));
    port.offline.store(true, Ordering::SeqCst);
    std::fs::write(wt.join("c.txt"), "c\n").unwrap();
    for _ in 0..5 {
        assert!(!k.core.tx().lc_try_close(&i).unwrap());
        std::thread::sleep(Duration::from_millis(50));
    }
    k.core.tx().lc_tick().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(k.phase(&i), "closing", "an unknown is never a change");
    assert!(
        !k.timeline_texts(&i)
            .iter()
            .any(|t| t.contains("冻结之后结果又变了"))
    );
    /* back: the change is seen */
    port.offline.store(false, Ordering::SeqCst);
    k.core.tx().lc_tick().unwrap();
    assert!(k.wait(|| k.phase(&i) == "active"));
}

/// A release project for `r`, its script waiting for `$STEVE_HOME/go`.
fn release_project(k: &Kit, r: &Path) -> String {
    let script = k.home().join("release.sh");
    std::fs::write(&script, super::fake_release_script("exit 0\n")).unwrap();
    let rel = Release {
        kind: "script".into(),
        cmd: script.to_string_lossy().into(),
        done_when: None,
    };
    k.db()
        .project_create(
            WS,
            "R",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap()
        .id
}

fn release_card(k: &Kit, i: &str, s: &str, branch: &str) -> String {
    let d = k.decision(i, Some(s), "approve", "release", &["yes", "no"]);
    k.db()
        .exec(
            "UPDATE decisions SET subject_id = ? WHERE id = ?",
            &[&branch, &d],
        )
        .unwrap();
    d
}

fn outcome(k: &Kit, d: &str) -> Value {
    k.db().decision(d).unwrap().unwrap().result
}

#[test]
fn head_moving_between_the_facts_and_the_rebase_refuses_it_and_nothing_is_released() {
    let k = Kit::new();
    let port = OnNode::new(&k);
    let r = repo(k.home());
    let p = release_project(&k, &r);
    let i = k.item("wait", "收尾", Some(&p));
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &i, "b.txt", "b\n");
    std::fs::write(r.join("c.txt"), "c\n").unwrap();
    g(&r, &["add", "."]);
    g(&r, &["commit", "-qm", "main c"]);
    k.db()
        .item_patch(
            &i,
            patch![cwd: wt.to_string_lossy().to_string(), branch: name.as_str()],
        )
        .unwrap();
    let s = k.session(&i, "idle");
    port.put(&i);

    /* the agent commits again after Steve looked and before the rebase */
    let moved = wt.clone();
    *port.before_apply.lock().unwrap() = Some(Box::new(move || {
        std::fs::write(moved.join("b.txt"), "b2\n").unwrap();
        g(&moved, &["commit", "-qam", "b2"]);
    }));
    let head = g(&wt, &["rev-parse", "HEAD"]);
    let d = release_card(&k, &i, &s, &name);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    assert!(k.wait(|| outcome(&k, &d)["outcome"] == "not_released"));
    assert_eq!(
        outcome(&k, &d)["reason"],
        json!("rebase 到 main 失败（核对之后分支又动了）")
    );
    assert!(k.core.tx().st.releasing.is_none(), "nothing released");
    assert_ne!(g(&wt, &["rev-parse", "HEAD"]), head);
    g(&wt, &["merge-base", "--is-ancestor", "HEAD~1", "HEAD"]);
    assert!(
        !crate::git::git_test(&wt, &["merge-base", "--is-ancestor", "main", "HEAD"]).unwrap(),
        "not rebased"
    );
    assert!(
        k.user_texts(&s)
            .iter()
            .any(|t| t.starts_with("发布前 rebase 到 main 失败（核对之后分支又动了）")),
        "the agent is sent back: {:?}",
        k.user_texts(&s)
    );

    /* asked again on the facts as they are: rebased on the Node, the release starts */
    let d2 = release_card(&k, &i, &s, &name);
    k.core.resolve(&d2, "yes", "", "me").unwrap();
    assert!(k.wait(|| outcome(&k, &d2)["outcome"] == "releasing"));
    assert!(crate::git::git_test(&wt, &["merge-base", "--is-ancestor", "main", "HEAD"]).unwrap());
    assert!(
        k.timeline_texts(&i)
            .iter()
            .any(|t| t == &format!("已把 {name} rebase 到最新的 main"))
    );
    assert_eq!(port.applies.lock().unwrap().len(), 2);
}
