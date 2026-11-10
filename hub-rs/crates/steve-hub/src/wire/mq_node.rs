//! The merge queue's Node (E1, S3): [`steve_core::mq::exec::MqNode`] on the Node that holds the
//! target's project. The staging is a [`PrepareKind::Staging`] workspace there (`mq:<target>`): a
//! worktree of the Node's own mirror of the project's repository, never of the repository itself.
//! What changes something (the reset for a batch, each merge, each check, the push) is written to
//! the Node's ledger under its operation id before it is sent, subject `mq:<target>`, so it runs at
//! most once; asked again (a restart), it answers what it did. Reads (where the target is, which
//! files changed) are the Node's git facts.
//!
//! The upstream is fetched where the project is (its own remote and credentials, as any fetch of
//! Steve's); the push goes from the staging to the target's canonical URL with the workspace's
//! GitHub token as a header, over the base it was integrated on (a lease).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use steve_core::mq::exec::{Checked, Cut, Merged, MqNode, Pushed, Staging, Step};
use steve_core::mq::host::MqHost;
use steve_node_api::*;

use super::flow_cmd::{command, dir_digest, prepare_dir, resource_of};
use super::git_push::HeaderValue;
use super::node_runtime::LedgerEntry;
use super::node_secrets::NodeSecrets;
use super::workspaces::{At, Placement, PlacementError, Subject, Workspaces};

/// How long preparing the staging (a mirror filled the first time) may take.
const PREPARE_WAIT: Duration = Duration::from_secs(10 * 60);
/// How long a reset or a merge may take to come back.
const STEP_WAIT: Duration = Duration::from_secs(6 * 60);
/// Past a check's own deadline, how long its answer may take to come back.
const CHECK_GRACE: Duration = Duration::from_secs(60);
/// What a check's output keeps (each stream).
const CHECK_OUTPUT: u64 = 64 * 1024;
/// The id the push's credential goes by, and the header it goes in.
const SECRET_ID: &str = "github";
const HEADER: &str = "AUTHORIZATION";

pub struct MqNodes {
    workspaces: Arc<Workspaces>,
    secrets: Arc<NodeSecrets>,
    /// Where a push's token comes from (the project's GitHub connection), once wired; none: pushes
    /// go without one (a repository on this machine).
    host: std::sync::OnceLock<Arc<dyn MqHost>>,
    /// How long to wait for a push's answer (tests shorten it).
    push_wait: Duration,
}

impl MqNodes {
    /// The host that gives pushes their token.
    pub fn set_host(&self, h: Arc<dyn MqHost>) {
        let _ = self.host.set(h);
    }

    pub fn new(workspaces: Arc<Workspaces>, secrets: Arc<NodeSecrets>) -> Arc<MqNodes> {
        Arc::new(MqNodes {
            workspaces,
            secrets,
            host: std::sync::OnceLock::new(),
            push_wait: Duration::from_secs(6 * 60),
        })
    }
}

fn subject(s: &Staging) -> String {
    format!("mq:{}", s.target_id)
}

/// Where the project is, and its repository as a workspace (for fetching and reading the target).
struct Placed {
    p: Placement,
    dir: RootPath,
    project: WorkspaceRef,
}

impl MqNodes {
    async fn placed(&self, s: &Staging) -> Result<Placed, String> {
        let w = self.workspaces.clone();
        tokio::task::spawn_blocking(move || w.refresh_roots())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let p = match self
            .workspaces
            .placement_of(Subject::Project(&s.project_id))
        {
            Ok(p) => p,
            Err(PlacementError::LocalOff) => {
                return Err("the merge queue needs the Hub's own Node (STEVE_NODE_MODE)".into());
            }
            Err(e) => return Err(e.to_string()),
        };
        let At::Root(dir) = p.at.clone() else {
            return Err("the project is not a directory on its Node".into());
        };
        let project = prepare_dir(&p.node, resource_of(&s.project_id, &dir), dir.clone()).await?;
        Ok(Placed { p, dir, project })
    }

    /// The staging workspace of the target (the same one every time).
    async fn staging(&self, s: &Staging, at: &Placed) -> Result<WorkspaceRef, String> {
        let c = command(
            &steve_store::uid("op_mqprep"),
            Prepare {
                resource: format!("mq:{}:{}", s.target_id, dir_digest(&at.dir)),
                kind: PrepareKind::Staging {
                    source: at.dir.clone(),
                    branch: s.branch.clone(),
                },
            },
            PREPARE_WAIT,
        )?;
        let until = Instant::now() + PREPARE_WAIT;
        let mut r = loop {
            match at.p.node.prepare_workspace(c.clone()).await {
                Err(e) if e.code == ErrorCode::Busy && Instant::now() < until => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                other => break other.map_err(|e| e.to_string())?,
            }
        };
        while !r.state.is_finished() {
            if Instant::now() >= until {
                return Err("the staging was not ready in time".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            r =
                at.p.node
                    .operation(c.id.clone())
                    .await
                    .map_err(|e| e.to_string())?;
        }
        match r.state {
            Observed::Finished {
                result: Some(OperationResult::Workspace(v)),
                ..
            } => Ok(v.workspace),
            Observed::Finished { error: Some(e), .. } => Err(e.to_string()),
            other => Err(format!("prepare: {:?}", other.tag())),
        }
    }

    async fn facts(node: &Arc<dyn Node>, w: &WorkspaceRef, q: GitQuery) -> ApiResult<GitFacts> {
        node.git_facts(GitQuery {
            workspace: w.clone(),
            ..q
        })
        .await
    }

    /// The settled entry of `op`, written now as `method` with `body()` when the ledger has none
    /// (asked again: the one there is, never a second one). `None`: not settled within `wait`.
    async fn ledgered<B: serde::Serialize>(
        &self,
        p: &Placement,
        op: &str,
        subject: &str,
        method: &str,
        body: impl std::future::Future<Output = Result<B, String>>,
        wait: Duration,
    ) -> Result<Option<LedgerEntry>, String> {
        let runtime = &p.runtime;
        if runtime.ledger(op).map_err(|e| e.to_string())?.is_none() {
            let body = body.await?;
            runtime
                .submit(op, subject, method, body)
                .map_err(|e| e.to_string())?;
        }
        Ok(Self::settled(p, op, wait).await)
    }

    async fn settled(p: &Placement, op: &str, wait: Duration) -> Option<LedgerEntry> {
        let until = Instant::now() + wait;
        loop {
            match p.runtime.ledger(op) {
                Ok(Some(l)) if l.is_settled() => return Some(l),
                Ok(_) => {}
                Err(e) => tracing::warn!("mq {op}: {e}"),
            }
            if Instant::now() >= until {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A git apply in the staging on the facts as they are now.
    async fn apply(
        &self,
        s: &Staging,
        op: &str,
        action: GitAction,
        wait: Duration,
    ) -> Result<(Placed, WorkspaceRef, Option<LedgerEntry>), String> {
        let at = self.placed(s).await?;
        let w = self.staging(s, &at).await?;
        let node = at.p.node.clone();
        let w2 = w.clone();
        let body = async move {
            let f = Self::facts(
                &node,
                &w2,
                GitQuery {
                    workspace: w2.clone(),
                    refs: vec![],
                    ranges: vec![],
                    fetch: false,
                    reads: vec![],
                    heads_only: false,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok(GitApply {
                workspace: w2,
                expected: f.revision,
                expected_head: f.head,
                action,
            })
        };
        let l = self
            .ledgered(&at.p, op, &subject(s), "git_apply", body, wait)
            .await?;
        Ok((at, w, l))
    }
}

/// How a settled git apply ended: its facts' HEAD, or the error.
fn applied(l: &LedgerEntry) -> Result<String, (ErrorCode, String)> {
    if l.state == "rejected" {
        let e = l.refusal.clone();
        return Err((
            ErrorCode::Stale,
            e.map(|e| e.safe_detail).unwrap_or_else(|| "refused".into()),
        ));
    }
    match l.receipt.as_ref().map(|r| &r.state) {
        Some(Observed::Finished {
            outcome: Outcome::Succeeded,
            result: Some(OperationResult::Git(f)),
            ..
        }) => Ok(f.head.clone()),
        Some(Observed::Finished {
            outcome: Outcome::Failed,
            error: Some(e),
            ..
        }) => Err((e.code, e.safe_detail.clone())),
        _ => Err((ErrorCode::OutcomeUnknown, l.state.clone())),
    }
}

#[async_trait]
impl MqNode for MqNodes {
    async fn ready(&self, s: &Staging) -> Result<String, String> {
        self.placed(s).await.map(|at| at.p.node_id.to_string())
    }

    async fn stage(&self, s: &Staging, op: &str) -> Step<String> {
        // the target as its host has it now, fetched where the project is (once per batch run:
        // asked again, the reset answers what it did)
        if let Ok(at) = self.placed(s).await
            && at.p.runtime.ledger(op).ok().flatten().is_none()
        {
            let r = Self::facts(
                &at.p.node,
                &at.project,
                GitQuery {
                    workspace: at.project.clone(),
                    refs: vec![],
                    ranges: vec![],
                    fetch: true,
                    reads: vec![],
                    heads_only: true,
                },
            )
            .await;
            if let Err(e) = r {
                return Step::Failed(e.to_string());
            }
        }
        let keep = s.keep.iter().filter_map(|k| RelPath::new(k).ok()).collect();
        let to = format!("refs/remotes/origin/{}", s.branch);
        match self
            .apply(s, op, GitAction::Reset { to, keep }, STEP_WAIT)
            .await
        {
            Err(e) => Step::Failed(e),
            Ok((_, _, None)) => Step::Unknown("the reset did not come back in time".into()),
            Ok((_, _, Some(l))) => match applied(&l) {
                Ok(head) => Step::Done(head),
                Err((ErrorCode::OutcomeUnknown, w)) => Step::Unknown(w),
                Err((_, w)) => Step::Failed(w),
            },
        }
    }

    async fn reset(&self, s: &Staging, op: &str, to: &str) -> Step<String> {
        let keep = s
            .keep
            .iter()
            .filter_map(|k| RelPath::new(k).ok())
            .collect();
        match self
            .apply(
                s,
                op,
                GitAction::Reset {
                    to: to.into(),
                    keep,
                },
                STEP_WAIT,
            )
            .await
        {
            Err(e) => Step::Failed(e),
            Ok((_, _, None)) => Step::Unknown("the reset did not come back in time".into()),
            Ok((_, _, Some(l))) => match applied(&l) {
                Ok(head) => Step::Done(head),
                Err((ErrorCode::OutcomeUnknown, w)) => Step::Unknown(w),
                Err((_, w)) => Step::Failed(w),
            },
        }
    }

    async fn merge(&self, s: &Staging, op: &str, commit: &str, message: &str) -> Merged {
        let action = GitAction::Merge {
            commit: commit.into(),
            message: Some(message.into()),
            back_out_on_conflict: true,
        };
        match self.apply(s, op, action, STEP_WAIT).await {
            Err(e) => Merged::Unknown(e),
            Ok((_, _, None)) => Merged::Unknown("the merge did not come back in time".into()),
            Ok((_, _, Some(l))) => match applied(&l) {
                Ok(head) => Merged::Done(head),
                Err((ErrorCode::Conflict, paths)) => {
                    Merged::Conflict(paths.lines().map(String::from).collect())
                }
                // refused before it ran (the staging moved): integrated again, it is local
                Err((ErrorCode::Stale | ErrorCode::OutcomeUnknown | ErrorCode::Busy, w)) => {
                    Merged::Unknown(w)
                }
                Err((_, w)) => Merged::Failed(w),
            },
        }
    }

    async fn changed(&self, s: &Staging, from: &str, to: &str) -> Step<Vec<String>> {
        let r = async {
            let at = self.placed(s).await?;
            let w = self.staging(s, &at).await?;
            let f = Self::facts(
                &at.p.node,
                &w,
                GitQuery {
                    workspace: w.clone(),
                    refs: vec![],
                    ranges: vec![],
                    fetch: false,
                    reads: vec![GitRead::ChangedPaths {
                        from: from.into(),
                        to: to.into(),
                        max: 10_000,
                    }],
                    heads_only: true,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            match f.reads.into_iter().next().map(|r| r.answer) {
                Some(GitAnswer::ChangedPaths { paths, .. }) => Ok(paths),
                _ => Err("changed paths".to_string()),
            }
        };
        match r.await {
            Ok(p) => Step::Done(p),
            Err(e) => Step::Failed(e),
        }
    }

    async fn present(&self, s: &Staging, rev: &str, paths: &[String]) -> Step<Vec<bool>> {
        let r = async {
            let at = self.placed(s).await?;
            let w = self.staging(s, &at).await?;
            let mut reads = vec![];
            for p in paths {
                reads.push(GitRead::Show {
                    rev: rev.into(),
                    path: Some(RelPath::new(p).map_err(|e| e.to_string())?),
                    max: 1,
                });
            }
            let f = Self::facts(
                &at.p.node,
                &w,
                GitQuery {
                    workspace: w.clone(),
                    refs: vec![],
                    ranges: vec![],
                    fetch: false,
                    reads,
                    heads_only: true,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(
                f.reads
                    .into_iter()
                    .map(|r| matches!(r.answer, GitAnswer::Blob { found: true, .. }))
                    .collect(),
            )
        };
        match r.await {
            Ok(v) => Step::Done(v),
            Err(e) => Step::Failed(e),
        }
    }

    async fn check(&self, s: &Staging, op: &str, cmd: &str, deadline: Duration) -> Checked {
        let r = async {
            let at = self.placed(s).await?;
            let w = self.staging(s, &at).await?;
            let end = steve_store::now() + deadline.as_millis() as i64;
            let spec = ExecSpec {
                workspace: w,
                command: cmd.into(),
                cwd: None,
                policy: ExecutionPolicy {
                    revision: Revision::new("mq-check").expect("constant id"),
                    permissions: PermissionMode::Ask,
                    sandbox: SandboxRequirement::None,
                    readable: vec![],
                    network: NetworkScope::Unrestricted,
                    tools: GrantId::new("hub").expect("constant id"),
                },
                limits: Limits {
                    deadline: Timestamp(end),
                    offline_until: Timestamp(end),
                    max_output_bytes: CHECK_OUTPUT,
                    max_tokens: None,
                    max_cost: None,
                },
                env: vec![],
                secrets: vec![],
            };
            self.ledgered(
                &at.p,
                op,
                &subject(s),
                "exec",
                async { Ok(spec) },
                deadline + CHECK_GRACE,
            )
            .await
        };
        let l = match r.await {
            Err(e) => return Checked::Unknown(e),
            Ok(None) => return Checked::Unknown("no answer in time".into()),
            Ok(Some(l)) => l,
        };
        if l.state == "rejected" {
            return Checked::Unknown(
                l.refusal
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "refused".into()),
            );
        }
        match l.receipt.as_ref().map(|r| &r.state) {
            Some(Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Exec(r)),
                ..
            }) => match r.exit_code {
                Some(0) => Checked::Passed,
                Some(_) => Checked::Failed(format!("{}{}", r.stdout, r.stderr)),
                None => Checked::Unknown("ended by a signal".into()),
            },
            Some(Observed::Finished {
                outcome: Outcome::TimedOut,
                ..
            }) => Checked::Unknown("past its deadline".into()),
            _ => Checked::Unknown(l.state.clone()),
        }
    }

    async fn push(&self, s: &Staging, op: &str, base: &str, tip: &str) -> Pushed {
        self.push_to(s, op, Some(base), tip, None).await
    }

    async fn push_branch(&self, s: &Staging, op: &str, tip: &str, to: &str) -> Pushed {
        self.push_to(s, op, None, tip, Some(to)).await
    }

    async fn remote(&self, s: &Staging) -> Option<String> {
        let at = self.placed(s).await.ok()?;
        let r = format!("refs/remotes/origin/{}", s.branch);
        let f = Self::facts(
            &at.p.node,
            &at.project,
            GitQuery {
                workspace: at.project.clone(),
                refs: vec![r],
                ranges: vec![],
                fetch: true,
                reads: vec![],
                heads_only: true,
            },
        )
        .await
        .ok()?;
        f.refs.into_iter().next().and_then(|(_, sha)| sha)
    }

    async fn cut(&self, s: &Staging, op: &str, ci_wait: Duration, deadline: Duration) -> Cut {
        // the staging at the target's tip, as fetched now; the script takes it from there
        let reset = derived_id("op_mqcutstage", op);
        match self.stage(s, &reset).await {
            Step::Done(_) => {}
            Step::Failed(w) => return Cut::Failed(w),
            Step::Unknown(w) => return Cut::Unknown(w),
        }
        let r = async {
            let at = self.placed(s).await?;
            let w = self.staging(s, &at).await?;
            // an older script without the CI gate takes no --ci-wait
            let f = Self::facts(
                &at.p.node,
                &w,
                GitQuery {
                    workspace: w.clone(),
                    refs: vec![],
                    ranges: vec![],
                    fetch: false,
                    reads: vec![GitRead::Show {
                        rev: "HEAD".into(),
                        path: Some(RelPath::new("ops/cut-release.py").map_err(|e| e.to_string())?),
                        max: 1 << 20,
                    }],
                    heads_only: true,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            let script = match f.reads.into_iter().next().map(|r| r.answer) {
                Some(GitAnswer::Blob {
                    found: true, bytes, ..
                }) => String::from_utf8_lossy(&bytes).into_owned(),
                _ => return Err("the repository has no ops/cut-release.py".to_string()),
            };
            let mut command = format!(
                "python3 ops/cut-release.py cut --branch '{}'",
                s.branch.replace('\'', "")
            );
            if script.contains("--ci-wait") {
                command.push_str(&format!(" --ci-wait {}", ci_wait.as_secs()));
            }
            let end = steve_store::now() + deadline.as_millis() as i64;
            let spec = ExecSpec {
                workspace: w,
                command,
                cwd: None,
                policy: ExecutionPolicy {
                    revision: Revision::new("mq-cut").expect("constant id"),
                    permissions: PermissionMode::Ask,
                    sandbox: SandboxRequirement::None,
                    readable: vec![],
                    network: NetworkScope::Unrestricted,
                    tools: GrantId::new("hub").expect("constant id"),
                },
                limits: Limits {
                    deadline: Timestamp(end),
                    offline_until: Timestamp(end),
                    max_output_bytes: CHECK_OUTPUT,
                    max_tokens: None,
                    max_cost: None,
                },
                env: vec![],
                secrets: vec![],
            };
            self.ledgered(
                &at.p,
                op,
                &subject(s),
                "exec",
                async { Ok(spec) },
                deadline + CHECK_GRACE,
            )
            .await
        };
        let l = match r.await {
            Err(e) => return Cut::Failed(e),
            Ok(None) => return Cut::Unknown("no answer in time".into()),
            Ok(Some(l)) => l,
        };
        if l.state == "rejected" {
            return Cut::Failed(
                l.refusal
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "refused".into()),
            );
        }
        match l.receipt.as_ref().map(|r| &r.state) {
            Some(Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Exec(r)),
                ..
            }) => match r.exit_code {
                Some(0) => Cut::Done(format!("{}{}", r.stdout, r.stderr)),
                Some(_) => Cut::Failed(format!("{}{}", r.stdout, r.stderr)),
                None => Cut::Unknown("ended by a signal".into()),
            },
            _ => Cut::Unknown(l.state.clone()),
        }
    }

    async fn tags(&self, s: &Staging) -> Option<Vec<(String, String)>> {
        let at = self.placed(s).await.ok()?;
        let f = Self::facts(
            &at.p.node,
            &at.project,
            GitQuery {
                workspace: at.project.clone(),
                refs: vec![],
                ranges: vec![],
                fetch: true,
                reads: vec![GitRead::RemoteTags {
                    remote: "origin".into(),
                }],
                heads_only: true,
            },
        )
        .await
        .ok()?;
        match f.reads.into_iter().next().map(|r| r.answer) {
            Some(GitAnswer::RemoteTags { tags }) => Some(tags),
            _ => None,
        }
    }

    async fn contains(&self, s: &Staging, ancestor: &str, of: &str) -> Option<bool> {
        let at = self.placed(s).await.ok()?;
        // read where the target was just fetched: a commit it does not have was never pushed there
        let f = Self::facts(
            &at.p.node,
            &at.project,
            GitQuery {
                workspace: at.project.clone(),
                refs: vec![],
                ranges: vec![],
                fetch: false,
                reads: vec![GitRead::Show {
                    rev: ancestor.into(),
                    path: None,
                    max: 1,
                }],
                heads_only: true,
            },
        )
        .await
        .ok()?;
        if matches!(
            f.reads.first().map(|r| &r.answer),
            Some(GitAnswer::Blob { found: false, .. })
        ) {
            return Some(false);
        }
        let f = Self::facts(
            &at.p.node,
            &at.project,
            GitQuery {
                workspace: at.project.clone(),
                refs: vec![],
                ranges: vec![],
                fetch: false,
                reads: vec![GitRead::IsAncestor {
                    ancestor: ancestor.into(),
                    of: of.into(),
                }],
                heads_only: true,
            },
        )
        .await
        .ok()?;
        match f.reads.first().map(|r| &r.answer) {
            Some(GitAnswer::IsAncestor { yes }) => Some(*yes),
            _ => None,
        }
    }
}

impl MqNodes {
    /// The staging's HEAD (`tip`) pushed to the target's branch over `base`, or to the branch `to`.
    async fn push_to(
        &self,
        s: &Staging,
        op: &str,
        base: Option<&str>,
        tip: &str,
        to: Option<&str>,
    ) -> Pushed {
        let at = match self.placed(s).await {
            Ok(a) => a,
            Err(e) => return Pushed::Failed(e),
        };
        let w = match self.staging(s, &at).await {
            Ok(w) => w,
            Err(e) => return Pushed::Failed(e),
        };
        let url = canonical_url(&s.upstream);
        let token = match self.host.get() {
            Some(h) => h.token(&s.project_id).await,
            None => None,
        };
        let runtime = at.p.runtime.clone();
        let known = runtime.ledger(op).ok().flatten();
        if known.is_none() {
            let f = match Self::facts(
                &at.p.node,
                &w,
                GitQuery {
                    workspace: w.clone(),
                    refs: vec![format!("refs/heads/{}", s.branch)],
                    ranges: vec![],
                    fetch: false,
                    reads: vec![],
                    heads_only: true,
                },
            )
            .await
            {
                Ok(f) => f,
                Err(e) => return Pushed::Failed(e.to_string()),
            };
            let apply = GitApply {
                workspace: w.clone(),
                expected: f.revision,
                expected_head: f.head,
                action: GitAction::Push {
                    remote: "origin".into(),
                    branch: s.branch.clone(),
                    expected_remote: base.map(String::from),
                    url: url.clone(),
                    origin_must_be: github_repo(&url),
                    expected_local: tip.into(),
                    auth: token.as_ref().map(|_| SecretRef {
                        id: SECRET_ID.into(),
                        how: SecretDelivery::Header {
                            name: HEADER.into(),
                        },
                    }),
                    remote_branch: to.map(String::from),
                },
            };
            let sub = subject(s);
            let r = match &token {
                Some(t) => self
                    .secrets
                    .submit_push(
                        op,
                        at.p.node_id.clone(),
                        vec![SECRET_ID.into()],
                        Arc::new(HeaderValue::of(t)),
                        || {
                            runtime
                                .ledger(op)
                                .map(|e| e.is_none_or(|e| !e.is_settled()))
                        },
                        || runtime.submit(op, &sub, "git_apply", apply),
                    )
                    .map(|_| ()),
                None => runtime.submit(op, &sub, "git_apply", apply).map(|_| ()),
            };
            if let Err(e) = r {
                return Pushed::Failed(e.to_string());
            }
            if token.is_some() {
                self.secrets.watch_push(&runtime, op);
            }
        } else if let Some(t) = &token
            && known.as_ref().is_some_and(|l| !l.is_settled())
        {
            // written before a restart, not answered yet: what it may take is given again
            let _ = self.secrets.give_once(
                op,
                at.p.node_id.clone(),
                vec![SECRET_ID.into()],
                Arc::new(HeaderValue::of(t)),
                || {
                    runtime
                        .ledger(op)
                        .map(|e| e.is_some_and(|e| !e.is_settled()))
                },
            );
            self.secrets.watch_push(&runtime, op);
        }
        let Some(l) = Self::settled(&at.p, op, self.push_wait).await else {
            return Pushed::Unknown("no answer in time".into());
        };
        self.secrets.retire_push(op);
        pushed(&l)
    }
}

/// How a settled push ended.
fn pushed(l: &LedgerEntry) -> Pushed {
    if l.state == "rejected" {
        return Pushed::Failed(
            l.refusal
                .as_ref()
                .map(|e| e.safe_detail.clone())
                .unwrap_or_else(|| "refused".into()),
        );
    }
    match l.receipt.as_ref().map(|r| &r.state) {
        Some(Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }) => Pushed::Done,
        Some(Observed::Finished {
            outcome: Outcome::Failed,
            error: Some(e),
            ..
        }) => {
            let d = e.safe_detail.as_str();
            // the lease, or the remote ahead: the target moved, nothing was pushed
            if ["stale info", "fetch first", "non-fast-forward"]
                .iter()
                .any(|w| d.contains(w))
            {
                Pushed::Moved
            } else {
                Pushed::Failed(d.into())
            }
        }
        _ => Pushed::Unknown(l.state.clone()),
    }
}

/// What to push to for a target's upstream ([`steve_core::lane::target::normalize_url`]'s
/// spelling): `github.com/o/r` → its HTTPS URL, `file:/path` → the path.
pub fn canonical_url(upstream: &str) -> String {
    if let Some(p) = upstream.strip_prefix("file:") {
        return p.to_string();
    }
    if upstream.starts_with('/') || upstream.contains("://") {
        return upstream.to_string();
    }
    format!("https://{upstream}.git")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use steve_core::mq::exec::{Member, Record, run_batch};
    use steve_core::mq::policy::Policy;
    use steve_core::mq::queue::Outcome;
    use steve_store::{Store, StoreError};

    use super::super::node_runtime::{NodeRuntime, Sink};
    use super::super::node_tools::tests::Ends;
    use super::super::workspaces::LocalFace;
    use super::*;

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

    fn commit(dir: &Path, file: &str, body: &str) -> String {
        std::fs::write(dir.join(file), body).unwrap();
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-qm", file]);
        sh(dir, &["rev-parse", "HEAD"])
    }

    struct T {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        store: Store,
        mq: Arc<MqNodes>,
        heads: Vec<String>,
    }

    /// A Hub with its local Node on and the project `p1` in `root/work` (origin the bare
    /// `root/remotes/r.git`), with the branches `f1` (a new file), `f2` and `f3` (one line, two
    /// ways).
    fn hub() -> T {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        sh(&work, &["init", "-q", "-b", "main"]);
        commit(&work, "a.txt", "one\n");
        let bare = root.join("remotes/r.git");
        std::fs::create_dir_all(&bare).unwrap();
        sh(&bare, &["init", "-q", "--bare", "-b", "main"]);
        sh(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        sh(&work, &["push", "-q", "origin", "main"]);
        let mut heads = vec![];
        for (b, f, body) in [
            ("f1", "b.txt", "b\n"),
            ("f2", "a.txt", "two\n"),
            ("f3", "a.txt", "three\n"),
        ] {
            sh(&work, &["checkout", "-qb", b, "main"]);
            heads.push(commit(&work, f, body));
        }
        sh(&work, &["checkout", "-q", "main"]);
        let store = Store::open(root.join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                ceiling: steve_node::PolicyCeiling::unlimited(),
                node: NodeId::new("local").unwrap(),
                home: root.join("node"),
                acp: steve_acp::AcpConfig {
                    adapters: Default::default(),
                    declared: Default::default(),
                    node: "node".into(),
                    claude_executable: "/usr/bin/false".into(),
                    steve_home: root.join("steve"),
                    codex_home: root.join("codex"),
                    user_home: root.to_path_buf(),
                    version: "test".into(),
                },
                roots: vec![],
                capacity: Capacity {
                    sessions: 0,
                    jobs: 0,
                    execs: 4,
                },
                storage: Default::default(),
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
        let w = work.to_str().unwrap().to_string();
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "INSERT INTO projects(id,workspace_id,name,path,kind,device_id,created,updated) VALUES ('p1','default','p1',?1,'git','hub',1,1)",
                    [&w],
                )?;
                Ok(())
            })
            .unwrap();
        T {
            mq: MqNodes::new(ws, secrets),
            _tmp: tmp,
            root,
            store,
            heads,
        }
    }

    impl T {
        fn staging(&self) -> Staging {
            Staging {
                target_id: "mq_1".into(),
                project_id: "p1".into(),
                upstream: format!("file:{}", self.root.join("remotes/r.git").display()),
                branch: "main".into(),
                keep: vec!["target".into()],
            }
        }
        fn remote(&self) -> String {
            sh(&self.root.join("remotes/r.git"), &["rev-parse", "main"])
        }
        fn count(&self, sql: &str) -> i64 {
            self.store
                .reader(|c| c.query_row(sql, [], |r| r.get(0)).map_err(StoreError::from))
                .unwrap()
        }
    }

    fn member(id: &str, head: &str) -> Member {
        Member {
            id: id.into(),
            head: head.into(),
            label: id.into(),
            verified_base: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_batch_runs_on_the_projects_node_through_the_ledger() {
        let t = hub();
        let p = Policy {
            checks: vec!["test -f b.txt && echo fine".into()],
            ..Policy::default()
        };
        let members: Vec<Member> = ["A", "B", "C"]
            .iter()
            .zip(&t.heads)
            .map(|(id, h)| member(id, h))
            .collect();
        let seen = Mutex::new(vec![]);
        let rec = |r: Record| seen.lock().unwrap().push(r);
        let r = run_batch(t.mq.as_ref(), &t.staging(), 1, 0, &members, &p, true, &rec).await;
        let tip = match &r.outcome {
            Some(Outcome::Landed { commit }) => commit.clone(),
            other => panic!("{other:?} {:?}", seen.lock().unwrap()),
        };
        assert_eq!(t.remote(), tip);
        assert_eq!(r.left.len(), 1, "{:?}", r.left);
        assert_eq!(r.left[0].0, "C");
        assert_eq!(
            r.checks,
            [("test -f b.txt && echo fine".to_string(), "passed")]
        );
        // every change went through the ledger, for the target
        assert_eq!(
            t.count("SELECT COUNT(*) FROM node_commands WHERE json_extract(request,'$.subject')='mq:mq_1' AND method='exec'"),
            1
        );
        let pushes = "SELECT COUNT(*) FROM node_commands WHERE json_extract(request,'$.subject')='mq:mq_1' AND request LIKE '%\"type\":\"push\"%'";
        assert_eq!(t.count(pushes), 1);
        // the staging is the Node's own: its objects are not the project's
        let staging = std::fs::read_dir(t.root.join("node/mq"))
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        let common = PathBuf::from(sh(
            &staging,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ));
        assert!(!common.join("objects/info/alternates").exists());
        assert!(!common.starts_with(t.root.join("work")));
        // the same run asked again (after a restart): what each step did, nothing sent again
        let again = run_batch(
            t.mq.as_ref(),
            &t.staging(),
            1,
            0,
            &members,
            &p,
            true,
            &|_| {},
        )
        .await;
        assert_eq!(again.outcome, r.outcome);
        assert_eq!(t.count(pushes), 1);
        // a failing check on the next batch: its tail, nothing pushed
        let fail = Policy {
            checks: vec!["echo nope >&2; exit 4".into()],
            ..Policy::default()
        };
        let work = t.root.join("work");
        sh(&work, &["checkout", "-qb", "f4", "main"]);
        let h4 = commit(&work, "d.txt", "d\n");
        sh(&work, &["checkout", "-q", "main"]);
        let r = run_batch(
            t.mq.as_ref(),
            &t.staging(),
            2,
            0,
            &[member("D", &h4)],
            &fail,
            true,
            &|_| {},
        )
        .await;
        match r.outcome {
            Some(Outcome::Failed { tail }) => assert!(tail.contains("nope"), "{tail}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(t.remote(), tip);
        // a commit fed on its own branch (a host's merge queue, a batch's pull request)
        let st = t.staging();
        assert_eq!(t.mq.reset(&st, "op_mqt_reset", &h4).await, Step::Done(h4.clone()));
        assert_eq!(
            t.mq.push_branch(&st, "op_mqt_push", &h4, "steve/mq/main/MQ-9").await,
            Pushed::Done
        );
        assert_eq!(
            sh(&t.root.join("remotes/r.git"), &["rev-parse", "steve/mq/main/MQ-9"]),
            h4
        );
        assert_eq!(t.remote(), tip, "main untouched");
    }
}
