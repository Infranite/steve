//! Read-only planning for local Node workspaces. No mkdir, git probe or business mutation here.
//! Existing cwd wins, exactly as core's start path does; new work uses project metadata. The Node
//! verifies repository/path identity when preparing. Roots use the same stable path identity.
use super::node_runtime::Binding;
use std::path::{Path, PathBuf};
use steve_node_api::*;
use steve_store::{Store, StoreError};

pub(crate) fn root_id(path: &Path) -> Option<RootId> {
    if !path.is_absolute() {
        return None;
    }
    RootId::new(derived_id("host-root", path.to_str()?)).ok()
}
fn at(path: &str) -> Option<RootPath> {
    Some(RootPath {
        root: root_id(Path::new(path))?,
        rel: RelPath::new("").ok()?,
    })
}

/// A binding is only a plan. The coordinator must bind it before routing a new session. Imported,
/// ended, read-only, cross-workspace or unrecognized permission settings fail closed.
/// `perm`: the harness mode the session is meant to start in (the workspace approval policy; `None`: the asking mode). `allows`: whether the Node takes a directory (its roots); a directory it does not take
/// plans nothing, and the session is refused.
pub fn binding_for(
    store: &Store,
    session: &str,
    perm: Option<&str>,
    allows: &dyn Fn(&RootPath) -> bool,
) -> Option<Binding> {
    binding_with(store, session, perm, allows, None)
}

/// [`binding_for`] on a Node that makes the items' worktrees (`worktrees`: Steve's worktrees
/// directory, one of its roots). An item of a git project working on its own worktree there
/// (`<worktrees>/<item id>` on its branch, made or not yet) gets a worktree plan at that very place:
/// the Node makes it, adopts one that is there already only if it is that worktree, and makes it
/// again after a tidy. An item whose workspace the Node first prepared as an existing directory
/// keeps that plan (the same resource is never prepared otherwise).
pub fn binding_with(
    store: &Store,
    session: &str,
    perm: Option<&str>,
    allows: &dyn Fn(&RootPath) -> bool,
    worktrees: Option<&Path>,
) -> Option<Binding> {
    plan(store, session, perm, allows, worktrees).ok()
}

/// Why a session has no binding on the local Node ([`plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unbound {
    /// Its rows could not be read, or a name in them is no valid id: maybe only for now.
    Unreadable,
    /// No such session.
    Missing,
    Ended,
    /// Imported from a harness's history: read only.
    ReadOnly,
    /// Not a session of Steve's on this machine.
    NotHere,
    /// It belongs to no item, or its item is in another workspace (or gone).
    NoItem,
    /// Its item's project is archived.
    Archived,
    /// Its bot is gone, disabled or in another workspace.
    Bot,
    /// A permission mode the Node path cannot express.
    Mode(String),
    /// Its directory is not one the Node takes (none of its roots).
    OutsideRoots,
}

impl Unbound {
    /// Whether this holds for good: a session that is not bound now never will be, as it is.
    /// An unreadable row may read fine the next time.
    pub fn is_permanent(&self) -> bool {
        !matches!(self, Unbound::Unreadable)
    }
}

/// [`binding_with`], saying why when there is no binding.
pub fn plan(
    store: &Store,
    session: &str,
    perm: Option<&str>,
    allows: &dyn Fn(&RootPath) -> bool,
    worktrees: Option<&Path>,
) -> Result<Binding, Unbound> {
    use Unbound::*;
    let s = store
        .sessions()
        .get(session)
        .map_err(|_| Unreadable)?
        .ok_or(Missing)?;
    if s.status == "ended" {
        return Err(Ended);
    }
    if s.readonly != 0 {
        return Err(ReadOnly);
    }
    if s.device_id != "hub" || s.source != "Steve" {
        return Err(NotHere);
    }
    let item = store
        .items()
        .get(s.item_id.as_deref().ok_or(NoItem)?)
        .map_err(|_| Unreadable)?
        .ok_or(NoItem)?;
    if item.workspace_id != s.workspace_id {
        return Err(NoItem);
    }
    // an archived project's directories are no roots any more ([`roots`]), its items' included
    if let Some(pid) = &item.project_id
        && store
            .projects()
            .get(pid)
            .map_err(|_| Unreadable)?
            .is_some_and(|p| p.archived)
    {
        return Err(Archived);
    }
    let bot = match s.bot_id.as_deref() {
        Some(id) => Some(store.bots().get(id).map_err(|_| Unreadable)?.ok_or(Bot)?),
        None => None,
    };
    if let Some(b) = &bot
        && (!b.enabled
            || !store
                .bots()
                .in_workspace(&b.id, &s.workspace_id)
                .map_err(|_| Unreadable)?)
    {
        return Err(Bot);
    }
    // A policy can replace the Bot's default, not hide an unrecognized stored setting. The
    // migration resets former widening settings; malformed data still fails closed on binding.
    if let Some(b) = &bot
        && !b.perm_mode.is_empty()
        && PermissionMode::from_key(&b.perm_mode).is_none()
    {
        return Err(Mode(b.perm_mode.clone()));
    }
    let mode = perm.unwrap_or("default");
    let permissions = PermissionMode::from_key(mode).ok_or_else(|| Mode(mode.to_string()))?;
    let cwd = if !s.cwd.is_empty() {
        Some(s.cwd.as_str())
    } else {
        item.cwd.as_deref().filter(|p| !p.is_empty())
    };
    let resource = format!("item:{}", item.id);
    let original = prepared_as(store, &resource).map_err(|_| Unreadable)?;
    let prepare = if let Some(cwd) = cwd {
        // Never replace an existing workspace containing user changes with a fresh worktree.
        let path = Path::new(cwd);
        let dir = if path.file_name().and_then(|n| n.to_str()) == Some(item.id.as_str())
            && (item.project_id.is_none() || item.branch.is_some())
        {
            RootPath {
                root: path.parent().and_then(root_id).ok_or(OutsideRoots)?,
                rel: RelPath::new(item.id.clone()).map_err(|_| Unreadable)?,
            }
        } else {
            at(cwd).ok_or(OutsideRoots)?
        };
        let existing = PrepareKind::Existing { dir };
        match worktrees.and_then(|w| item_worktree(store, &item, &s.workspace_id, path, w)) {
            Some(wt) if original.as_ref().is_none_or(|k| *k != existing) => wt,
            _ => existing,
        }
    } else if let Some(pid) = &item.project_id {
        let p = store
            .projects()
            .get(pid)
            .map_err(|_| Unreadable)?
            .ok_or(NoItem)?;
        if p.workspace_id != s.workspace_id || p.device_id != "hub" {
            return Err(NotHere);
        }
        if p.archived {
            return Err(Archived);
        }
        if p.kind == "git" && p.worktree {
            PrepareKind::Worktree {
                repository: at(&p.path).ok_or(OutsideRoots)?,
                branch: format!("steve/{}", item.id.to_lowercase()),
                base: None,
                at: match worktrees {
                    Some(w) => Some(RootPath {
                        root: root_id(w).ok_or(OutsideRoots)?,
                        rel: RelPath::new(item.id.clone()).map_err(|_| Unreadable)?,
                    }),
                    None => None,
                },
            }
        } else {
            PrepareKind::Existing {
                dir: at(&p.path).ok_or(OutsideRoots)?,
            }
        }
    } else {
        PrepareKind::Scratch
    };
    match &prepare {
        PrepareKind::Existing { dir: at } | PrepareKind::Worktree { repository: at, .. } => {
            if !allows(at) {
                return Err(OutsideRoots);
            }
        }
        PrepareKind::Scratch => {}
    }
    // A resource is immutable on the Node. A project/directory change gets a deterministic
    // identity of its own; retries use the same identity and never rewrite the old workspace.
    let resource =
        workspace_resource(&resource, original.as_ref(), &prepare).map_err(|_| Unreadable)?;
    Ok(Binding {
        session: s.id,
        resource,
        prepare,
        choice: HarnessChoice {
            harness: s.harness,
            model: s.model,
            effort: s.reasoning_effort,
        },
        policy: ExecutionPolicy {
            revision: Revision::new(format!("host-{}", bot.as_ref().map_or(0, |b| b.revision)))
                .map_err(|_| Unreadable)?,
            permissions,
            sandbox: SandboxRequirement::None,
            readable: vec![],
            network: NetworkScope::Unrestricted,
            tools: GrantId::new("hub").map_err(|_| Unreadable)?,
        },
    })
}

/// The worktree plan of an item working at `cwd` when that is its own worktree in `worktrees`
/// (`<worktrees>/<item id>`, on its branch, of its git project on the Hub's device).
fn item_worktree(
    store: &Store,
    item: &steve_store::Item,
    workspace: &str,
    cwd: &Path,
    worktrees: &Path,
) -> Option<PrepareKind> {
    let branch = item.branch.as_deref().filter(|b| !b.is_empty())?;
    if cwd != worktrees.join(&item.id) {
        return None;
    }
    let p = store.projects().get(item.project_id.as_deref()?).ok()??;
    if p.workspace_id != workspace
        || p.device_id != "hub"
        || p.archived
        || p.kind != "git"
        || !p.worktree
    {
        return None;
    }
    Some(PrepareKind::Worktree {
        repository: at(&p.path)?,
        branch: branch.to_string(),
        base: None,
        at: Some(RootPath {
            root: root_id(worktrees)?,
            rel: RelPath::new(item.id.clone()).ok()?,
        }),
    })
}

/// The first specification sent for the item's base resource. Later failed or pending requests
/// must not redefine that immutable identity.
fn prepared_as(store: &Store, resource: &str) -> Result<Option<PrepareKind>, StoreError> {
    store.read(|r| steve_store::node_sessions::worktree::first_prepare(r, "local", resource))
}

fn workspace_resource(
    base: &str,
    original: Option<&PrepareKind>,
    desired: &PrepareKind,
) -> ApiResult<String> {
    if original.is_none_or(|kind| kind == desired) {
        return Ok(base.to_string());
    }
    Ok(format!("{base}:workspace:{}", digest_of(desired)?.as_str()))
}

/// The strict router already checked ownership, tag, project and item affinity, and checks
/// them once more in the bind transaction. Remote projects are never mapped to scratch.
pub(super) fn scoped_remote_plan(
    store: &Store,
    session: &str,
    perm: Option<&str>,
) -> Option<Binding> {
    let mut plan = binding_for(store, session, perm, &|_| true)?;
    plan.prepare = PrepareKind::Scratch;
    Some(plan)
}

/// Why a paired remote Node may not get a workspace's work ([`remote_refusal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteRefusal {
    /// Unknown, pending, paused or revoked.
    Inactive,
    /// The user who paired it does not own the workspace.
    NotOwner,
    /// The workspace has members besides the Node's owner, so a run there may be any of theirs.
    Shared,
}

impl RemoteRefusal {
    /// Why, in the current language (inside [`steve_i18n::stored`] for stored text).
    pub fn why(&self) -> String {
        match self {
            RemoteRefusal::Inactive => steve_i18n::t!("hub.workspaces.offline"),
            RemoteRefusal::NotOwner => steve_i18n::t!("hub.node.remote.not_owner"),
            RemoteRefusal::Shared => steve_i18n::t!("hub.node.remote.shared"),
        }
    }
}

/// Whether the paired remote Node `node` may get `workspace`'s work now, and why not (H1). A
/// remote Node runs its sessions, jobs and commands with its owner's own home and logins, unsandboxed
/// and under whatever policy the Hub sends, so until runs are sandboxed it takes only its owner's
/// own work: it is active, the user who paired it (`nodes.owner`) owns the workspace, and nobody
/// else is a member of it. The Hub records no user per run (an agent works for the workspace's
/// owner, [`super::mcp_extra::owner`]), and any member of a workspace can start or steer a run in
/// it, so the only runs that are surely the owner's are those of a workspace that is theirs alone.
pub(crate) fn remote_refusal(
    r: &mut impl steve_store::Read,
    node: &str,
    workspace: &str,
) -> Result<Option<RemoteRefusal>, StoreError> {
    let admission = steve_store::node_directory::workspace_admission(r, node, workspace)?;
    Ok(match admission {
        None => Some(RemoteRefusal::Inactive),
        Some(a) if !a.active => Some(RemoteRefusal::Inactive),
        Some(a) if !a.owner => Some(RemoteRefusal::NotOwner),
        Some(a) if a.shared => Some(RemoteRefusal::Shared),
        Some(_) => None,
    })
}

/// Whether the paired remote Node `node` may get `workspace`'s work: [`remote_refusal`] has nothing
/// against it.
pub(crate) fn node_takes(
    c: &mut impl steve_store::Read,
    node: &str,
    workspace: &str,
) -> Result<bool, StoreError> {
    Ok(remote_refusal(c, node, workspace)?.is_none())
}

/// The local Node's allowlist: explicit configured roots (`extra`: the scratch and worktree
/// directories) plus project and local session directories, read-write, and knowledge folders,
/// read-only. No filesystem traversal and never `/`. A path that is both stays read-write. Built at
/// boot and again whenever projects or knowledge sources change ([`super::workspaces`]), so a
/// project added after boot binds without a restart and an archived one is refused (with the
/// directories of its items and their sessions).
pub(crate) fn roots(store: &Store, extra: &[PathBuf]) -> Result<Vec<steve_node::Root>, StoreError> {
    let (rw, ro) = store.read(|r| steve_store::node_sessions::placements::roots(r))?;
    let mut rw: Vec<PathBuf> = rw.into_iter().map(PathBuf::from).collect();
    rw.extend(extra.iter().cloned());
    rw.sort();
    rw.dedup();
    let mut ro: Vec<PathBuf> = ro.into_iter().map(PathBuf::from).collect();
    ro.sort();
    ro.dedup();
    ro.retain(|p| rw.binary_search(p).is_err());
    let root = |path: PathBuf, access: RootAccess| {
        Some(steve_node::Root {
            id: root_id(&path)?,
            display: if access == RootAccess::ReadOnly {
                "knowledge folder".into()
            } else {
                "local workspace".into()
            },
            path,
            access,
        })
    };
    Ok(rw
        .into_iter()
        .map(|p| (p, RootAccess::ReadWrite))
        .chain(ro.into_iter().map(|p| (p, RootAccess::ReadOnly)))
        .filter(|(p, _)| p != Path::new("/"))
        .filter_map(|(p, a)| root(p, a))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::node_tools::tests::fixture;
    use super::*;
    #[test]
    fn changed_workspace_specs_get_stable_distinct_resources() {
        let old = PrepareKind::Scratch;
        let next = PrepareKind::Existing {
            dir: at("/project").unwrap(),
        };
        assert_eq!(
            workspace_resource("item:i1", None, &old).unwrap(),
            "item:i1"
        );
        assert_eq!(
            workspace_resource("item:i1", Some(&old), &old).unwrap(),
            "item:i1"
        );
        let resource = workspace_resource("item:i1", Some(&old), &next).unwrap();
        assert_ne!(resource, "item:i1");
        assert_eq!(
            resource,
            workspace_resource("item:i1", Some(&old), &next).unwrap()
        );
        assert_ne!(
            resource,
            workspace_resource(
                "item:i1",
                Some(&old),
                &PrepareKind::Existing {
                    dir: at("/other").unwrap()
                }
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn a_failed_later_spec_never_redefines_a_base_resource() {
        let h = fixture(vec![]).await;
        for (n, kind) in [
            (1, PrepareKind::Scratch),
            (
                2,
                PrepareKind::Existing {
                    dir: at("/project").unwrap(),
                },
            ),
        ] {
            let req = serde_json::to_string(&Command {
                id: OperationId::new(format!("prep{n}")).unwrap(),
                issued_at: Timestamp(1),
                accept_before: Timestamp(2),
                grant: GrantId::new("hub").unwrap(),
                body: Prepare {
                    resource: "item:i1".into(),
                    kind,
                },
            })
            .unwrap();
            steve_store::testkit::hub::node_planning::seed_rejected_prepare(
                &h.store,
                &format!("prep{n}"),
                &req,
            )
            .unwrap();
        }
        assert_eq!(
            prepared_as(&h.store, "item:i1").unwrap(),
            Some(PrepareKind::Scratch)
        );
    }

    #[tokio::test]
    async fn planning_is_pure_and_preserves_cwd() {
        let h = fixture(vec![]).await;
        assert!(matches!(
            binding_for(&h.store, "s1", None, &|_| true)
                .unwrap()
                .prepare,
            PrepareKind::Scratch
        ));
        let project = h.dir.path().join("project");
        let cwd = h.dir.path().join("worktrees/i1");
        steve_store::testkit::hub::node_fixtures::seed_projects_and_set_items_project_id_p1(
            &h.store,
            project.to_str().unwrap(),
        )
        .unwrap();
        assert!(
            matches!(binding_for(&h.store, "s1", None, &|_| true).unwrap().prepare,PrepareKind::Worktree {branch,..} if branch=="steve/i1")
        );
        assert!(
            !project.exists(),
            "planning must not create or probe a repo"
        );
        steve_store::testkit::hub::node_fixtures::set_sessions_and_set_items_branch_steve(
            &h.store,
            cwd.to_str().unwrap(),
        )
        .unwrap();
        let b = binding_for(&h.store, "s1", None, &|_| true).unwrap();
        assert!(
            matches!(b.prepare,PrepareKind::Existing {dir} if dir.root==root_id(cwd.parent().unwrap()).unwrap() && dir.rel.as_str()=="i1")
        );
        assert!(!cwd.exists());
        steve_store::testkit::hub::node_fixtures::set_sessions_readonly(&h.store).unwrap();
        assert!(binding_for(&h.store, "s1", None, &|_| true).is_none());
    }

    /// An item the Node first prepared as an existing directory keeps that plan, also once its
    /// worktree is gone: the core makes it before such a session is bound (only a worktree plan
    /// is one the Node makes).
    #[tokio::test]
    async fn an_item_first_prepared_as_existing_keeps_it_with_its_worktree_gone() {
        let h = fixture(vec![]).await;
        let project = h.dir.path().join("project");
        let worktrees = h.dir.path().join("worktrees");
        let cwd = worktrees.join("i1");
        steve_store::testkit::hub::node_fixtures::seed_existing_item_project(
            &h.store,
            project.to_str().unwrap(),
            cwd.to_str().unwrap(),
            cwd.to_str().unwrap(),
        )
        .unwrap();
        let plan = || binding_with(&h.store, "s1", None, &|_| true, Some(&worktrees)).unwrap();
        assert!(matches!(plan().prepare, PrepareKind::Worktree { .. }));
        let existing = PrepareKind::Existing {
            dir: RootPath {
                root: root_id(&worktrees).unwrap(),
                rel: RelPath::new("i1").unwrap(),
            },
        };
        let c = Command {
            id: OperationId::new("prepare:s0:r0").unwrap(),
            issued_at: Timestamp(1),
            accept_before: Timestamp(2),
            grant: GrantId::new("hub").unwrap(),
            body: Prepare {
                resource: "item:i1".into(),
                kind: existing.clone(),
            },
        };
        let req = serde_json::to_string(&c).unwrap();
        steve_store::testkit::hub::workspace_cases::record_existing_prepare(
            &h.store, &existing, &req,
        )
        .unwrap();
        assert!(!cwd.exists());
        assert_eq!(plan().prepare, existing);
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    #[test]
    fn a_remote_node_takes_only_the_work_of_its_owners_own_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let cwd = dir.path().join("scratch/i1");
        steve_store::testkit::hub::workspace_cases::seed_remote_item(&store, cwd.to_str().unwrap())
            .unwrap();
        let why = |n: &str| store.read(|c| remote_refusal(c, n, "default")).unwrap();
        assert_eq!(why("n_a"), None);
        assert_eq!(why("n_m"), Some(RemoteRefusal::NotOwner));
        assert_eq!(why("n_p"), Some(RemoteRefusal::Inactive));
        assert_eq!(why("n_none"), Some(RemoteRefusal::Inactive));
        steve_store::testkit::hub::mutations::seed_workspace_members(&store).unwrap();
        assert_eq!(why("n_a"), Some(RemoteRefusal::Shared));
    }
}

#[cfg(test)]
mod bot_tests {
    use super::*;

    /// The seeded "Steve 开发" bot runs in `auto`: its sessions go to the Node in that mode.
    #[test]
    fn the_steve_dev_bot_binds_in_auto() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        steve_store::testkit::hub::mutations::seed_bots_for_the_steve_dev_bot_binds_in_auto(&store)
            .unwrap();
        let b =
            binding_for(&store, "s1", Some("auto"), &|_| true).expect("the Steve 开发 bot binds");
        assert_eq!(b.policy.permissions, PermissionMode::Auto);
        assert_eq!(b.policy.revision.as_str(), "host-3");
    }

    #[test]
    fn binding_requires_current_membership_and_enabled_bot_not_its_home_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        steve_store::testkit::hub::mutations::seed_workspaces(&store).unwrap();
        let bind = || plan(&store, "s1", None, &|_| true, None);
        assert_eq!(
            bind()
                .expect("a member can serve outside its home")
                .policy
                .revision
                .as_str(),
            "host-2"
        );
        store
            .bots()
            .set_workspaces("shared", &["home".into()])
            .unwrap();
        assert!(matches!(bind(), Err(Unbound::Bot)));
        store
            .bots()
            .set_workspaces("shared", &["home".into(), "default".into()])
            .unwrap();
        assert!(bind().is_ok());
        store
            .bots()
            .patch(
                "shared",
                &steve_store::BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "test",
            )
            .unwrap();
        assert!(matches!(bind(), Err(Unbound::Bot)));
    }
}

#[cfg(test)]
mod host_tests {
    use super::super::node_tools::tests::Ends;
    use super::super::{Vars, node_tools};
    use super::*;
    use std::sync::Arc;
    struct NoTools;
    #[async_trait::async_trait]
    impl node_tools::Service for NoTools {
        fn catalog(&self) -> Vec<ToolDescriptor> {
            vec![]
        }
        async fn call(
            &self,
            _: steve_store::Session,
            _: &str,
            _: serde_json::Value,
        ) -> Result<String, String> {
            Err("no tools".into())
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn diagnostic_and_real_local_host_with_fake_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::config::Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(dir.path().join("steve").to_string_lossy().into()),
            "STEVE_REPO" => Some(dir.path().join("unused").to_string_lossy().into()),
            _ => None,
        })
        .unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let mut jobs = vec![];
        let unset: Vars = Arc::new(|_| None);
        cfg.mode = crate::config::RuntimeMode::Diagnostic;
        assert!(
            super::super::node_runtime(
                &cfg,
                &store,
                Arc::new(Ends),
                &unset,
                &super::super::node_secrets::NodeSecrets::new(),
                &mut jobs
            )
            .unwrap()
            .is_none()
        );
        cfg.mode = crate::config::RuntimeMode::Normal;
        assert!(jobs.is_empty());
        assert!(!cfg.data_dir.join("node").exists());
        let on: Vars = Arc::new(|k| (k == "STEVE_NODE_MODE").then(|| "local".into()));
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
        cfg.adapters = std::collections::BTreeMap::from([(
            "claude-code".into(),
            fake.to_string_lossy().into(),
        )]);
        cfg.claude_executable = "/usr/bin/false".into();
        steve_store::testkit::hub::node_fixtures::seed_items_and_sessions_for_turn_ended(&store)
            .unwrap();
        let super::super::NodeHost {
            runtime: rt,
            tools,
            node,
            ..
        } = super::super::node_runtime(
            &cfg,
            &store,
            Arc::new(Ends),
            &on,
            &super::super::node_secrets::NodeSecrets::new(),
            &mut jobs,
        )
        .unwrap()
        .unwrap();
        tools.install(Arc::new(NoTools)).unwrap();
        for job in jobs {
            job.spawn().await;
        }
        let b = binding_for(&store, "s1", None, &|p| node.allows(p)).unwrap();
        rt.bind(&b).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let phase =
                steve_store::testkit::hub::node_checks::binding_phase_by_session(&store, "s1")
                    .unwrap();
            if phase == "ready" {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "open stuck: {phase}");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        rt.prompt("s1", "@say host wired", "user", &[]).unwrap();
        loop {
            let done = steve_store::testkit::hub::node_fixtures::host_node_texts(&store).unwrap();
            if done > 0 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no transcript");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // the Hub's shutdown stops the local Node, and its adapter's process group with it
        let pgid = steve_store::testkit::hub::node_journal::process_group(
            &cfg.data_dir.join("node/node.sqlite3"),
            "s1",
        )
        .unwrap();
        let alive = || unsafe { libc::kill(-pgid, 0) } == 0;
        assert!(alive());
        // what CoreDomain::shutdown awaits for it
        tokio::time::timeout(std::time::Duration::from_secs(10), node.shutdown())
            .await
            .expect("the Node stops within its deadline");
        let gone = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            !alive()
        });
        if !gone {
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        assert!(gone, "the adapter outlived the Hub's shutdown");
    }

    /// A new session the local Node cannot run is refused, said why, instead of becoming a dead
    /// Node session: a project directory added after the Node started (not among its roots)
    /// plans no binding, and once the Node stopped its bind is refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_the_local_node_cannot_run_is_refused() {
        use steve_core::AgentRuntime;
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(dir.path().join("steve").to_string_lossy().into()),
            "STEVE_REPO" => Some(dir.path().join("unused").to_string_lossy().into()),
            _ => None,
        })
        .unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .bots()
            .seed(
                "steve",
                true,
                &steve_store::NewBot {
                    name: "Steve".into(),
                    harness: "claude-code".into(),
                    ..Default::default()
                },
                steve_store::WS,
            )
            .unwrap();
        let on: Vars = Arc::new(|k| (k == "STEVE_NODE_MODE").then(|| "local".into()));
        let mut jobs = vec![];
        let super::super::NodeHost {
            runtime: rt,
            tools,
            node,
            ..
        } = super::super::node_runtime(
            &cfg,
            &store,
            Arc::new(Ends),
            &on,
            &super::super::node_secrets::NodeSecrets::new(),
            &mut jobs,
        )
        .unwrap()
        .unwrap();
        tools.install(Arc::new(NoTools)).unwrap();
        for job in jobs {
            job.spawn().await;
        }
        let host = Arc::new(super::super::core::HubHost {
            store: store.clone(),
            data_dir: cfg.data_dir.clone(),
            core: Default::default(),
            flow: Default::default(),
            approval: Default::default(),
            local_node: Default::default(),
        });
        let _ = host.local_node.set(node.clone());
        let routed = super::super::core::RoutedRuntime {
            node: Some(rt.clone()),
            fleet: None,
            store: store.clone(),
            host,
            // no local face: the roots are not built again before a bind, so a directory added
            // after boot is still outside them (what `allows` guards)
            workspaces: super::super::workspaces::Workspaces::new(
                store.clone(),
                None,
                None,
                vec![],
            ),
            worktrees: None,
            dispatch_harnesses: Some(crate::harnesses::ready_locally(&["claude-code"], &[]).await),
        };
        let bound = |sid: &str| -> i64 {
            steve_store::testkit::hub::node_fixtures::binding_count(&store, &sid).unwrap()
        };
        // a project made after boot: its directory exists but is not a root of this Node
        let project = dir.path().join("later");
        std::fs::create_dir_all(&project).unwrap();
        steve_store::testkit::hub::node_fixtures::seed_projects_and_items(
            &store,
            project.to_str().unwrap(),
        )
        .unwrap();
        assert!(binding_for(&store, "s1", None, &|_| true).is_some());
        assert!(binding_for(&store, "s1", None, &|p| node.allows(p)).is_none());
        routed.created("s1", None);
        assert_eq!(
            bound("s1"),
            0,
            "a directory outside the roots is bound nowhere"
        );
        let why = routed.refusal("s1").expect("refused");
        assert!(why.contains("不在 Node 可用的目录里"), "{why}");
        assert!(
            routed.prompt("s1", "做事", "me", &[]).is_err(),
            "never run on the Hub"
        );
        // a scratch session could run there, but the Node stopped
        assert!(binding_for(&store, "s2", None, &|p| node.allows(p)).is_some());
        rt.stop("its tools did not connect");
        routed.created("s2", None);
        assert_eq!(bound("s2"), 0, "a stopped Node binds nothing");
        let why = routed.refusal("s2").expect("refused");
        assert!(why.contains("离线"), "{why}");
        assert!(!super::super::node_runtime::bound(&store, "s2"));
        node.shutdown().await;
    }

    #[test]
    fn permission_modes_map_and_unknown_ones_fail_closed() {
        assert_eq!(PermissionMode::from_key("plan"), Some(PermissionMode::Plan));
        assert_eq!(
            PermissionMode::from_key("default"),
            Some(PermissionMode::Ask)
        );
        assert_eq!(PermissionMode::from_key("ask"), Some(PermissionMode::Ask));
        assert_eq!(
            PermissionMode::from_key("acceptEdits"),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            PermissionMode::from_key("accept_edits"),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            PermissionMode::from_key("bypassPermissions"),
            Some(PermissionMode::FullAccess)
        );
        assert_eq!(
            PermissionMode::from_key("full_access"),
            Some(PermissionMode::FullAccess)
        );
        assert_eq!(PermissionMode::from_key("auto"), Some(PermissionMode::Auto));
        assert_eq!(PermissionMode::from_key(""), None);
        assert_eq!(PermissionMode::from_key("dontAsk"), None);
    }
}
