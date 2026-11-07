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

/// Steve's permission mode for a harness or bot mode name. `None`: the Node path cannot express it,
/// fail closed.
pub(crate) fn permission_mode(perm: &str) -> Option<PermissionMode> {
    Some(match perm {
        "plan" => PermissionMode::Plan,
        "default" | "ask" => PermissionMode::Ask,
        "acceptEdits" | "accept_edits" => PermissionMode::AcceptEdits,
        "auto" => PermissionMode::Auto,
        "bypassPermissions" | "full_access" => PermissionMode::FullAccess,
        _ => return None,
    })
}

/// A binding is only a plan. The coordinator must bind it before routing a new session. Imported,
/// ended, read-only, cross-workspace or unrecognized permission settings fail closed.
/// `perm`: the harness mode the session is meant to start in (the bot's `perm_mode` as YOLO sets
/// it, `Core::session_perm_mode`; `None`: the harness default), the same the legacy runner gave
/// it. `allows`: whether the Node takes a directory (its roots); a directory it does not take
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
    // This is a pure plan, not permission to dispatch it: RoutedRuntime's workflow preflight
    // also requires a verified execution principal before a binding can be persisted. Keep the
    // requested sandbox explicit so a future Node cannot silently interpret ACP modes as one.
    let sandbox = if item.source.1 == "workflow"
        || store
            .workflows()
            .by_child(&item.id)
            .map_err(|_| Unreadable)?
            .is_some()
    {
        SandboxRequirement::Required {
            guarantees: vec!["os-sandbox".into(), "principal-isolation".into()],
        }
    } else {
        SandboxRequirement::None
    };
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
    let mode = perm.unwrap_or("default");
    let permissions = permission_mode(mode).ok_or_else(|| Mode(mode.to_string()))?;
    let cwd = if !s.cwd.is_empty() {
        Some(s.cwd.as_str())
    } else {
        item.cwd.as_deref().filter(|p| !p.is_empty())
    };
    let resource = format!("item:{}", item.id);
    let prepare = if let Some(cwd) = cwd {
        // Never replace a legacy workspace containing user changes with a fresh worktree.
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
            Some(wt) if prepared_as(store, &resource).is_none_or(|k| k != existing) => wt,
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
            sandbox,
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

/// How the local Node last prepared `resource`, as the Hub asked it (`None`: never).
fn prepared_as(store: &Store, resource: &str) -> Option<PrepareKind> {
    let request: String = store
        .reader(|c| {
            use rusqlite::OptionalExtension;
            Ok::<_, StoreError>(
                c.query_row(
                    "SELECT request FROM node_commands WHERE node_id='local' AND method='prepare_workspace'
                      AND json_extract(request,'$.body.resource')=? ORDER BY rowid DESC LIMIT 1",
                    [resource],
                    |r| r.get(0),
                )
                .optional()?,
            )
        })
        .ok()??;
    serde_json::from_str::<Command<Prepare>>(&request)
        .ok()
        .map(|c| c.body.kind)
}

/// The binding of a new session to the paired remote Node `node` (`STEVE_NODE_BIND`), or `None`
/// when it must stay where it would go otherwise. A remote Node has none of the Hub's directories,
/// so only an item without a project qualifies, in a fresh scratch workspace on that Node, and only
/// while every earlier session of the item ran there too (its files are there, not on the Hub).
/// The Node must be paired and active (a paused or revoked Node takes no new work), and take the
/// workspace's work ([`remote_refusal`]: paired by its owner, who is its only member): the Node
/// runs the session with its owner's own logins, unsandboxed, so it only ever runs its owner's work.
/// `speaks_auto`: the Node is known to speak [`AUTO_MODE`]; a session in `auto` goes to it only
/// then (an older Node would refuse to open it, for good), and otherwise stays where it would go.
/// `perm`: as for [`binding_for`].
pub fn remote_binding_for(
    store: &Store,
    session: &str,
    perm: Option<&str>,
    node: &NodeId,
    speaks_auto: bool,
) -> Option<Binding> {
    // the Hub's directories are not planned there: the workspace is a fresh scratch one
    let mut b = binding_for(store, session, perm, &|_| true)?;
    if b.policy.permissions == PermissionMode::Auto && !speaks_auto {
        return None;
    }
    let s = store.sessions().get(session).ok()??;
    let item = store.items().get(s.item_id.as_deref()?).ok()??;
    if item.project_id.is_some() {
        return None;
    }
    let ok = store
        .reader(|c| {
            let takes = node_takes(c, node.as_str(), &s.workspace_id)?;
            // earlier sessions of the item that are not on this Node (on the Hub, or elsewhere)
            let elsewhere: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions x LEFT JOIN runtime_sessions r ON r.session_id=x.id
                  WHERE x.item_id=?1 AND x.id<>?2 AND (r.node_id IS NULL OR r.node_id<>?3))",
                rusqlite::params![item.id, session, node.as_str()],
                |r| r.get(0),
            )?;
            Ok::<_, StoreError>(takes && !elsewhere)
        })
        .ok()?;
    if !ok {
        return None;
    }
    b.prepare = PrepareKind::Scratch;
    Some(b)
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
    c: &rusqlite::Connection,
    node: &str,
    workspace: &str,
) -> Result<Option<RemoteRefusal>, StoreError> {
    use rusqlite::OptionalExtension;
    let row: Option<(String, bool, i64)> = c
        .query_row(
            "SELECT n.state,
                    EXISTS(SELECT 1 FROM workspace_members m WHERE m.workspace_id=?2 AND m.user_id=n.owner AND m.role='owner'),
                    (SELECT COUNT(*) FROM workspace_members m WHERE m.workspace_id=?2 AND m.user_id<>n.owner)
               FROM nodes n WHERE n.node_id=?1",
            rusqlite::params![node, workspace],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(match row {
        None => Some(RemoteRefusal::Inactive),
        Some((state, _, _)) if state != "active" => Some(RemoteRefusal::Inactive),
        Some((_, false, _)) => Some(RemoteRefusal::NotOwner),
        Some((_, true, others)) if others > 0 => Some(RemoteRefusal::Shared),
        Some(_) => None,
    })
}

/// Whether the paired remote Node `node` may get `workspace`'s work: [`remote_refusal`] has nothing
/// against it.
pub(crate) fn node_takes(
    c: &rusqlite::Connection,
    node: &str,
    workspace: &str,
) -> Result<bool, StoreError> {
    Ok(remote_refusal(c, node, workspace)?.is_none())
}

/// The directories [`roots`] gives the local Node, before the configured ones: every workspace's
/// projects, session and item directories (read-write) and knowledge folders (read-only).
pub(crate) fn root_paths_in(
    c: &rusqlite::Connection,
) -> Result<(Vec<String>, Vec<String>), StoreError> {
    let mut q = c.prepare("SELECT path FROM projects WHERE archived=0 AND device_id='hub'
         UNION SELECT s.cwd FROM sessions s LEFT JOIN items i ON i.id=s.item_id LEFT JOIN projects p ON p.id=i.project_id
          WHERE s.status<>'ended' AND s.readonly=0 AND s.source='Steve' AND s.device_id='hub' AND (p.id IS NULL OR p.archived=0)
         UNION SELECT i.cwd FROM items i LEFT JOIN projects p ON p.id=i.project_id
          WHERE i.cwd IS NOT NULL AND (p.id IS NULL OR p.archived=0)")?;
    let rw = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut q = c.prepare("SELECT json_extract(config,'$.path') FROM kb_sources WHERE kind='folder' AND json_type(config,'$.path')='text'")?;
    let ro = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((rw, ro))
}

/// The local Node's allowlist: explicit configured roots (`extra`: the scratch and worktree
/// directories) plus project and local session directories, read-write, and knowledge folders,
/// read-only. No filesystem traversal and never `/`. A path that is both stays read-write. Built at
/// boot and again whenever projects or knowledge sources change ([`super::workspaces`]), so a
/// project added after boot binds without a restart and an archived one is refused (with the
/// directories of its items and their sessions).
pub(crate) fn roots(store: &Store, extra: &[PathBuf]) -> Result<Vec<steve_node::Root>, StoreError> {
    let (rw, ro) = store.reader(root_paths_in)?;
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
        h.store.transaction::<_,StoreError>(|tx|{
            tx.execute("INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','project',?,'git',1,1,1)",[project.to_str().unwrap()])?;
            tx.execute("UPDATE items SET project_id='p1' WHERE id='i1'",[])?;Ok(())
        }).unwrap();
        assert!(
            matches!(binding_for(&h.store, "s1", None, &|_| true).unwrap().prepare,PrepareKind::Worktree {branch,..} if branch=="steve/i1")
        );
        assert!(
            !project.exists(),
            "planning must not create or probe a repo"
        );
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "UPDATE sessions SET cwd=? WHERE id='s1'",
                    [cwd.to_str().unwrap()],
                )?;
                tx.execute("UPDATE items SET branch='steve/i1' WHERE id='i1'", [])?;
                Ok(())
            })
            .unwrap();
        let b = binding_for(&h.store, "s1", None, &|_| true).unwrap();
        assert!(
            matches!(b.prepare,PrepareKind::Existing {dir} if dir.root==root_id(cwd.parent().unwrap()).unwrap() && dir.rel.as_str()=="i1")
        );
        assert!(!cwd.exists());
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("UPDATE sessions SET readonly=1 WHERE id='s1'", [])?;
                Ok(())
            })
            .unwrap();
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
        h.store.transaction::<_,StoreError>(|tx|{
            tx.execute("INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','project',?,'git',1,1,1)",[project.to_str().unwrap()])?;
            tx.execute("UPDATE items SET project_id='p1', branch='steve/i1', cwd=?1 WHERE id='i1'",[cwd.to_str().unwrap()])?;
            tx.execute("UPDATE sessions SET cwd=? WHERE id='s1'",[cwd.to_str().unwrap()])?;
            Ok(())
        }).unwrap();
        let plan = || binding_with(&h.store, "s1", None, &|_| true, Some(&worktrees)).unwrap();
        assert!(matches!(plan().prepare, PrepareKind::Worktree { .. }));
        let existing = PrepareKind::Existing {
            dir: RootPath {
                root: root_id(&worktrees).unwrap(),
                rel: RelPath::new("i1").unwrap(),
            },
        };
        let c = Command {
            id: OperationId::new("op_prep_s0").unwrap(),
            issued_at: Timestamp(1),
            accept_before: Timestamp(2),
            grant: GrantId::new("hub").unwrap(),
            body: Prepare {
                resource: "item:i1".into(),
                kind: existing.clone(),
            },
        };
        let req = serde_json::to_string(&c).unwrap();
        h.store.transaction::<_,StoreError>(|tx|{
            tx.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,created,updated) VALUES('op_prep_s0','local','prepare_workspace',?,'d','finished',1,1)",[req])?;
            Ok(())
        }).unwrap();
        assert!(!cwd.exists());
        assert_eq!(plan().prepare, existing);
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    fn exec(store: &Store, sql: &str) {
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn only_project_less_items_whose_sessions_all_ran_there_go_to_an_active_remote_node() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let cwd = dir.path().join("scratch/i1");
        exec(
            &store,
            &format!(
                "INSERT INTO items(id,workspace_id,no,title,status,stage,cwd,created,updated) VALUES ('i1','default',1,'t','run','执行','{c}',1,1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','{c}','active',1,1);
                 INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','sub_u','u','owner',1),('m','default','sub_m','m','member',1);
                 INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_a','u','a',zeroblob(32),'fa','active',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_p','u','p',zeroblob(32),'fp','paused',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_m','m','m',zeroblob(32),'fm','active',1);",
                c = cwd.display()
            ),
        );
        let a = NodeId::new("n_a").unwrap();
        // the Hub's directory is not the Node's: a fresh scratch workspace there
        let b = remote_binding_for(&store, "s1", None, &a, true).unwrap();
        assert!(matches!(b.prepare, PrepareKind::Scratch));
        assert!(matches!(
            binding_for(&store, "s1", None, &|_| true).unwrap().prepare,
            PrepareKind::Existing { .. }
        ));
        // a paused or unknown Node takes no new work
        assert!(
            remote_binding_for(&store, "s1", None, &NodeId::new("n_p").unwrap(), true).is_none()
        );
        assert!(
            remote_binding_for(&store, "s1", None, &NodeId::new("n_none").unwrap(), true).is_none()
        );
        // a Node a plain member paired never gets the workspace's sessions
        assert!(
            remote_binding_for(&store, "s1", None, &NodeId::new("n_m").unwrap(), true).is_none()
        );
        // an earlier session of the item on the Hub keeps the item's work there
        exec(
            &store,
            "INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s0','default','i1','claude-code','','ended',0,0);",
        );
        assert!(remote_binding_for(&store, "s1", None, &a, true).is_none());
        // unless it ran on that very Node
        exec(
            &store,
            "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES ('s0','n_a','{}','s0','r','released',0);",
        );
        assert!(remote_binding_for(&store, "s1", None, &a, true).is_some());
        // H1: another member in the workspace, and the run may be theirs: not on the owner's Node
        exec(
            &store,
            "INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','m','member',2);",
        );
        assert!(remote_binding_for(&store, "s1", None, &a, true).is_none());
        let why = |n: &str| store.reader(|c| remote_refusal(c, n, "default")).unwrap();
        assert_eq!(why("n_a"), Some(RemoteRefusal::Shared));
        assert_eq!(why("n_m"), Some(RemoteRefusal::NotOwner));
        assert_eq!(why("n_p"), Some(RemoteRefusal::Inactive));
        assert_eq!(why("n_none"), Some(RemoteRefusal::Inactive));
        exec(&store, "DELETE FROM workspace_members WHERE user_id='m';");
        assert_eq!(why("n_a"), None);
        // a project's directories are on the Hub
        exec(
            &store,
            "INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','p','/tmp/p','git',1,1,1);
             UPDATE items SET project_id='p1' WHERE id='i1';",
        );
        assert!(remote_binding_for(&store, "s1", None, &a, true).is_none());
    }
}

#[cfg(test)]
mod bot_tests {
    use super::*;

    /// The seeded "Steve 开发" bot runs in `auto`: its sessions go to the Node in that mode.
    #[test]
    fn the_steve_dev_bot_binds_in_auto() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute_batch(
                    "INSERT INTO bots(id,workspace_id,name,harness,perm_mode,revision,updated) VALUES ('dev','default','Steve 开发','claude-code','auto',3,1);
                     INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('dev','default',1);
                     INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES ('i1','default',1,'t','run','执行','dev',1,1);
                     INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','dev','claude-code','','active',1,1);",
                )?;
                Ok(())
            })
            .unwrap();
        let b =
            binding_for(&store, "s1", Some("auto"), &|_| true).expect("the Steve 开发 bot binds");
        assert_eq!(b.policy.permissions, PermissionMode::Auto);
        assert_eq!(b.policy.revision.as_str(), "host-3");
    }

    #[test]
    fn binding_requires_current_membership_and_enabled_bot_not_its_home_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute_batch(
                    "INSERT INTO workspaces(id,name,created) VALUES ('home','Home',1);
                     INSERT INTO bots(id,workspace_id,name,harness,revision,updated) VALUES ('shared','home','Shared','fixture',2,1);
                     INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('shared','home',1),('shared','default',1);
                     INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES ('i1','default',1,'t','run','执行','shared',1,1);
                     INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','shared','fixture','','active',1,1);",
                )?;
                Ok(())
            })
            .unwrap();
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

    /// The Node path starts a session with the permissions the legacy runner gave it: the
    /// mode `Core::session_perm_mode` computes (the bot's, as YOLO widens it) for every bot mode ×
    /// YOLO level × directory, and a read-only session never binds.
    #[test]
    fn a_binding_starts_in_the_mode_the_legacy_runner_would() {
        use steve_core::yolo::agent::{Level, harness_mode, perm_mode};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let bots = [
            Some("plan"),
            Some("default"),
            Some("acceptEdits"),
            Some("auto"),
            Some("bypassPermissions"),
            None,
        ];
        let mut sql = String::new();
        for (n, bot) in bots.iter().enumerate() {
            let bot_id = match bot {
                Some(m) => {
                    sql += &format!(
                        "INSERT INTO bots(id,workspace_id,name,harness,perm_mode,revision,updated) VALUES ('b{n}','default','b{n}','claude-code','{m}',1,1);
                         INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('b{n}','default',1);"
                    );
                    format!("'b{n}'")
                }
                None => "NULL".into(),
            };
            for ro in [0, 1] {
                sql += &format!(
                    "INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES ('i{n}_{ro}','default',{no},'t','run','执行',{bot_id},1,1);
                     INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,readonly,created,updated) VALUES ('s{n}_{ro}','default','i{n}_{ro}',{bot_id},'claude-code','','active',{ro},1,1);",
                    no = n * 2 + ro + 1
                );
            }
        }
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute_batch(&sql)?;
                Ok(())
            })
            .unwrap();
        let levels = [
            (Level::Off, false),
            (Level::On, true),
            (Level::On, false),
            (Level::Full, false),
        ];
        for (n, bot) in bots.iter().enumerate() {
            for (level, own) in levels {
                for ro in [false, true] {
                    // what Core::session_perm_mode gives (HubHost::perm_mode)
                    let legacy = if ro {
                        perm_mode(*bot, false)
                    } else {
                        harness_mode(*bot, level, own)
                    };
                    let sid = format!("s{n}_{}", ro as u8);
                    let b = binding_for(&store, &sid, legacy.as_deref(), &|_| true);
                    let case = format!("{bot:?} {level:?} own={own} readonly={ro}");
                    if ro {
                        assert!(b.is_none(), "{case}");
                        continue;
                    }
                    let want = permission_mode(legacy.as_deref().unwrap_or("default"));
                    assert_eq!(b.map(|b| b.policy.permissions), want, "{case}");
                }
            }
        }
        // the cases this used to get wrong or must keep: a bypass bot is not full access by itself
        let mode = |sid: &str, m: Option<String>| {
            binding_for(&store, sid, m.as_deref(), &|_| true).map(|b| b.policy.permissions)
        };
        let bypass = Some("bypassPermissions");
        assert_eq!(
            mode("s4_0", harness_mode(bypass, Level::Off, false)),
            Some(PermissionMode::Ask)
        );
        assert_eq!(
            mode("s4_0", harness_mode(bypass, Level::Full, false)),
            Some(PermissionMode::FullAccess)
        );
        assert_eq!(
            mode("s1_0", harness_mode(Some("default"), Level::On, true)),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            mode("s1_0", harness_mode(Some("default"), Level::On, false)),
            Some(PermissionMode::Ask)
        );
        assert_eq!(
            mode("s5_0", harness_mode(None, Level::Full, false)),
            Some(PermissionMode::FullAccess)
        );
    }

    /// A paired Node that does not speak `auto` (released before it, or not linked since the Hub
    /// started) never gets an `auto` Bot's session: it would refuse the open and keep refusing.
    #[test]
    fn an_auto_bot_session_goes_to_a_remote_node_only_if_it_speaks_auto() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute_batch(
                    "INSERT INTO bots(id,workspace_id,name,harness,perm_mode,revision,updated) VALUES ('dev','default','Steve 开发','claude-code','auto',3,1),('ask','default','b','claude-code','default',1,1);
                     INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('dev','default',1),('ask','default',1);
                     INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES ('i1','default',1,'t','run','执行','dev',1,1),('i2','default',2,'t','run','执行','ask',1,1);
                     INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','dev','claude-code','','active',1,1),('s2','default','i2','ask','claude-code','','active',1,1);
                     INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','sub_u','u','owner',1);
                     INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1);
                     INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_a','u','a',zeroblob(32),'fa','active',1);",
                )?;
                Ok(())
            })
            .unwrap();
        let a = NodeId::new("n_a").unwrap();
        assert!(remote_binding_for(&store, "s1", Some("auto"), &a, false).is_none());
        // other modes are unaffected
        assert!(remote_binding_for(&store, "s2", Some("default"), &a, false).is_some());
        let b = remote_binding_for(&store, "s1", Some("auto"), &a, true)
            .expect("a Node that speaks auto");
        assert_eq!(b.policy.permissions, PermissionMode::Auto);
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
    async fn explicit_off_and_real_local_host_with_fake_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::config::Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(dir.path().join("steve").to_string_lossy().into()),
            "STEVE_REPO" => Some(dir.path().join("unused").to_string_lossy().into()),
            _ => None,
        })
        .unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        // the rollback: STEVE_NODE_MODE=off builds no Node; neither does an unset mode on a
        // STEVE_NO_RESUME copy
        let off: Vars = Arc::new(|k| (k == "STEVE_NODE_MODE").then(|| "off".into()));
        let mut jobs = vec![];
        assert!(
            super::super::node_runtime(
                &cfg,
                &store,
                Arc::new(Ends),
                &off,
                &super::super::node_secrets::NodeSecrets::new(),
                &mut jobs
            )
            .unwrap()
            .is_none()
        );
        let unset: Vars = Arc::new(|_| None);
        cfg.no_resume = true;
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
        cfg.no_resume = false;
        assert!(jobs.is_empty());
        assert!(!cfg.data_dir.join("node").exists());
        let on: Vars = Arc::new(|k| (k == "STEVE_NODE_MODE").then(|| "local".into()));
        cfg.no_resume = true;
        assert!(
            super::super::node_runtime(
                &cfg,
                &store,
                Arc::new(Ends),
                &on,
                &super::super::node_secrets::NodeSecrets::new(),
                &mut jobs
            )
            .is_err()
        );
        cfg.no_resume = false;
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
        cfg.adapters = std::collections::BTreeMap::from([(
            "claude-code".into(),
            fake.to_string_lossy().into(),
        )]);
        cfg.claude_executable = "/usr/bin/false".into();
        store.transaction::<_,StoreError>(|tx| {
            tx.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)",[])?;
            tx.execute("INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','','active',1,1)",[])?;Ok(())
        }).unwrap();
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
            job.await;
        }
        let b = binding_for(&store, "s1", None, &|p| node.allows(p)).unwrap();
        rt.bind(&b).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let phase = store
                .reader(|c| {
                    Ok::<_, StoreError>(c.query_row(
                        "SELECT phase FROM runtime_sessions WHERE session_id='s1'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?)
                })
                .unwrap();
            if phase == "ready" {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "open stuck: {phase}");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        rt.prompt("s1", "@say host wired", "user", &[]).unwrap();
        loop {
            let done=store.reader(|c|Ok::<_,StoreError>(c.query_row("SELECT COUNT(*) FROM events WHERE format='steve.v1' AND data LIKE '%host wired%'",[],|r|r.get::<_,i64>(0))?)).unwrap();
            if done > 0 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no transcript");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // the Hub's shutdown stops the local Node, and its adapter's process group with it
        let journal = rusqlite::Connection::open(cfg.data_dir.join("node/node.sqlite3")).unwrap();
        let pgid: i32 = journal
            .query_row("SELECT pgid FROM procs WHERE session='s1'", [], |r| {
                r.get(0)
            })
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
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
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
            job.await;
        }
        let host = Arc::new(super::super::core::HubHost {
            store: store.clone(),
            data_dir: cfg.data_dir.clone(),
            core: Default::default(),
            flow: Default::default(),
            yolo: Default::default(),
            local_node: Default::default(),
        });
        let _ = host.local_node.set(node.clone());
        let routed = super::super::core::RoutedRuntime {
            node: Some(rt.clone()),
            fleet: None,
            bind_remote: None,
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
        };
        let bound = |sid: &str| -> i64 {
            store
                .reader(|c| {
                    Ok::<_, StoreError>(c.query_row(
                        "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?",
                        [sid],
                        |r| r.get(0),
                    )?)
                })
                .unwrap()
        };
        // a project made after boot: its directory exists but is not a root of this Node
        let project = dir.path().join("later");
        std::fs::create_dir_all(&project).unwrap();
        store.transaction::<_, StoreError>(|tx| {
            tx.execute("INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','later',?,'dir',0,1,1)",[project.to_str().unwrap()])?;
            tx.execute_batch("INSERT INTO items(id,workspace_id,no,title,status,stage,project_id,created,updated) VALUES ('i1','default',1,'t','run','执行','p1',1,1),('i2','default',2,'t','run','执行',NULL,1,1);
                INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','','active',1,1),('s2','default','i2','claude-code','','active',1,1);")?;
            Ok(())
        }).unwrap();
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
        assert!(why.contains("本机 Node 没有在运行"), "{why}");
        assert!(!super::super::node_runtime::bound(&store, "s2"));
        node.shutdown().await;
    }

    #[test]
    fn permission_modes_map_and_unknown_ones_fail_closed() {
        assert_eq!(permission_mode("plan"), Some(PermissionMode::Plan));
        assert_eq!(permission_mode("default"), Some(PermissionMode::Ask));
        assert_eq!(
            permission_mode("acceptEdits"),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            permission_mode("bypassPermissions"),
            Some(PermissionMode::FullAccess)
        );
        assert_eq!(permission_mode("auto"), Some(PermissionMode::Auto));
        assert_eq!(permission_mode(""), None);
        assert_eq!(permission_mode("dontAsk"), None);
    }

    /// Boot migration must use the same isolation gate as prompts, before a Node binding or
    /// workspace prepare. A retired attempt and a missing cwd must not turn a hold into closure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_adoption_parks_workflow_before_binding_or_workspace_access() {
        use steve_core::AgentRuntime;
        struct NoModel;
        #[async_trait::async_trait]
        impl steve_core::gateway::ModelGateway for NoModel {
            async fn call(
                &self,
                _: &str,
                _: steve_core::gateway::Tier,
                _: std::time::Duration,
                _: &steve_core::gateway::ThinkTools,
            ) -> steve_core::Result<steve_core::gateway::Printed> {
                panic!("adoption of blocked work must not call a model");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let cfg = crate::config::Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(dir.path().join("steve").to_string_lossy().into()),
            "STEVE_REPO" => Some(dir.path().join("unused").to_string_lossy().into()),
            _ => None,
        })
        .unwrap();
        store.transaction::<_, StoreError>(|tx| {
            tx.execute_batch(
                "INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
                 VALUES ('parent','default',601,'主事项','run','执行',1,1),
                        ('child','default',602,'工作单元','run','执行',1,1);
                 INSERT INTO item_goals(item_id,version,text,created)
                 VALUES ('parent',1,'验证启动迁移隔离',1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated)
                 VALUES ('work','default','child','unused-acp','','active',1,1);
                 INSERT INTO workflows(item_id,revision,max_parallel,max_attempts,created,updated)
                 VALUES ('parent',1,2,3,1,1);
                 INSERT INTO workflow_units(workflow_id,id,position,spec,status,item_id,session_id)
                 VALUES ('parent','work',0,'{\"id\":\"work\",\"title\":\"工作单元\",\"goal\":\"验证隔离\",\"bot_id\":\"test\"}','running','child','work');"
            )?;
            Ok(())
        }).unwrap();
        let on: Vars = Arc::new(|k| (k == "STEVE_NODE_MODE").then(|| "local".into()));
        let mut jobs = vec![];
        let super::super::NodeHost {
            runtime,
            tools,
            node,
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
            job.await;
        }
        let host = Arc::new(super::super::core::HubHost {
            store: store.clone(),
            data_dir: cfg.data_dir.clone(),
            core: Default::default(),
            flow: Default::default(),
            yolo: Default::default(),
            local_node: Default::default(),
        });
        let _ = host.local_node.set(node.clone());
        let routed = Arc::new(super::super::core::RoutedRuntime {
            node: Some(runtime),
            fleet: None,
            bind_remote: None,
            store: store.clone(),
            host,
            workspaces: super::super::workspaces::Workspaces::new(
                store.clone(),
                None,
                None,
                vec![],
            ),
            worktrees: None,
        });
        let core = steve_core::Core::new(
            steve_core::repo::Db::new(store.clone()),
            routed.clone(),
            Arc::new(NoModel),
            super::super::core_config(&cfg),
        );
        let missing = dir.path().join("unprepared-work");
        let journal = rusqlite::Connection::open(cfg.data_dir.join("node/node.sqlite3")).unwrap();
        for detached in [false, true] {
            if detached {
                store.transaction::<_, StoreError>(|tx| {
                    tx.execute("UPDATE items SET source='[\"协作工作\",\"workflow\"]' WHERE id='child'", [])?;
                    tx.execute("DELETE FROM workflow_units WHERE item_id='child'", [])?;
                    Ok(())
                }).unwrap();
            }
            for cwd in ["", missing.to_str().unwrap()] {
                store
                    .transaction::<_, StoreError>(|tx| {
                        tx.execute("UPDATE sessions SET cwd=? WHERE id='work'", [cwd])?;
                        Ok(())
                    })
                    .unwrap();
                for before in [None, Some(steve_store::now())] {
                    let moved = super::super::core::move_legacy(&routed, &core, before);
                    assert_eq!(moved.adopted, 0);
                    assert_eq!(moved.ended, 0);
                    assert_eq!(moved.left, 1);
                    assert!(!super::super::node_runtime::bound(&store, "work"));
                    assert!(
                        !routed.is_busy("work"),
                        "an unbound session holds no process"
                    );
                    assert!(matches!(
                        routed.release("work").unwrap(),
                        steve_core::Released::Gone
                    ));
                    assert_eq!(
                        store.sessions().get("work").unwrap().unwrap().status,
                        "active"
                    );
                    assert_eq!(
                        store.items().get("parent").unwrap().unwrap().phase,
                        "active"
                    );
                    for table in ["ops", "workspaces", "sessions", "procs"] {
                        let count: i64 = journal
                            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                            .unwrap();
                        assert_eq!(count, 0, "{table}: detached={detached}, cwd={cwd}");
                    }
                    assert!(!missing.exists(), "no work directory is prepared");
                }
            }
        }
        node.shutdown().await;
    }

    /// A work graph cannot adopt an unbound ACP session onto a Node without isolation. Check the
    /// real routed runtime, including a retired attempt whose row a replan no longer keeps.
    #[tokio::test]
    async fn workflow_sessions_require_isolation_and_never_fall_back() {
        use steve_core::AgentRuntime;

        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(dir.path().join("steve").to_string_lossy().into()),
            "STEVE_REPO" => Some(dir.path().join("unused").to_string_lossy().into()),
            _ => None,
        })
        .unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store.transaction::<_, StoreError>(|tx| {
            tx.execute_batch(
                "INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
                 VALUES ('parent','default',1,'主事项','run','执行',1,1),
                        ('child','default',2,'工作单元','run','执行',1,1);
                 INSERT INTO item_goals(item_id,version,text,created)
                 VALUES ('parent',1,'验证运行隔离',1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated)
                 VALUES ('work','default','child','unused-acp','','active',1,1);
                 INSERT INTO workflows(item_id,revision,max_parallel,max_attempts,created,updated)
                 VALUES ('parent',1,2,3,1,1);
                 INSERT INTO workflow_units(workflow_id,id,position,spec,status,item_id,session_id)
                 VALUES ('parent','work',0,'{\"id\":\"work\",\"title\":\"工作单元\",\"goal\":\"验证隔离\",\"bot_id\":\"test\"}','running','child','work');"
            )?;
            Ok(())
        }).unwrap();
        let host = Arc::new(super::super::core::HubHost {
            store: store.clone(),
            data_dir: cfg.data_dir.clone(),
            core: Default::default(),
            flow: Default::default(),
            yolo: Default::default(),
            local_node: Default::default(),
        });
        let routed = super::super::core::RoutedRuntime {
            node: None,
            fleet: None,
            bind_remote: None,
            store: store.clone(),
            host,
            workspaces: super::super::workspaces::Workspaces::new(
                store.clone(),
                None,
                None,
                vec![],
            ),
            worktrees: None,
        };
        for (change, restore, message) in [
            (
                "UPDATE items SET cycle=2 WHERE id='parent'",
                "UPDATE items SET cycle=1 WHERE id='parent'",
                "目标或周期已变化",
            ),
            (
                "UPDATE item_goals SET version=2 WHERE item_id='parent'",
                "UPDATE item_goals SET version=1 WHERE item_id='parent'",
                "目标或周期已变化",
            ),
            (
                "UPDATE workflows SET paused=1",
                "UPDATE workflows SET paused=0",
                "已暂停",
            ),
            (
                "UPDATE items SET status='pause' WHERE id='parent'",
                "UPDATE items SET status='run' WHERE id='parent'",
                "已暂停",
            ),
            (
                "UPDATE items SET phase='closed' WHERE id='parent'",
                "UPDATE items SET phase='active' WHERE id='parent'",
                "已结束",
            ),
            (
                "UPDATE items SET phase='closing',closing='{\"kind\":\"cancel\"}' WHERE id='parent'",
                "UPDATE items SET phase='active',closing=NULL WHERE id='parent'",
                "正在停止",
            ),
            (
                "UPDATE workflow_units SET status='succeeded'",
                "UPDATE workflow_units SET status='running'",
                "已经停止或交付",
            ),
            (
                "UPDATE workflow_units SET status='cancelled'",
                "UPDATE workflow_units SET status='running'",
                "已经停止或交付",
            ),
            (
                "UPDATE workflow_units SET session_id=NULL",
                "UPDATE workflow_units SET session_id='work'",
                "当前的执行会话",
            ),
            (
                "UPDATE sessions SET status='ended' WHERE id='work'",
                "UPDATE sessions SET status='active' WHERE id='work'",
                "当前的执行会话",
            ),
        ] {
            store
                .transaction::<_, StoreError>(|tx| {
                    tx.execute_batch(change)?;
                    Ok(())
                })
                .unwrap();
            assert!(
                routed
                    .prompt("work", "排队的下一回合", "user", &[])
                    .unwrap_err()
                    .to_string()
                    .contains(message),
                "{change}"
            );
            assert_eq!(routed.flush("work", None), steve_core::Flushed::Held);
            assert_eq!(routed.running_turns(), 0);
            store
                .transaction::<_, StoreError>(|tx| {
                    tx.execute_batch(restore)?;
                    Ok(())
                })
                .unwrap();
        }
        for detached in [false, true] {
            if detached {
                store.transaction::<_, StoreError>(|tx| {
                    tx.execute("UPDATE items SET source='[\"协作工作\",\"workflow\"]' WHERE id='child'", [])?;
                    tx.execute("DELETE FROM workflow_units WHERE item_id='child'", [])?;
                    Ok(())
                }).unwrap();
            }
            let planned = binding_for(&store, "work", None, &|_| true).unwrap();
            assert_eq!(
                planned.policy.sandbox,
                SandboxRequirement::Required {
                    guarantees: vec!["os-sandbox".into(), "principal-isolation".into()],
                },
            );
            routed.created("work", None);
            assert!(!super::super::node_runtime::bound(&store, "work"));
            let error = routed
                .prompt("work", "必须不能启动", "user", &[])
                .unwrap_err()
                .to_string();
            if detached {
                assert!(error.contains("已被替换"));
            } else {
                assert!(error.contains("OS 沙箱") && error.contains("主体"));
            }
            assert_eq!(routed.flush("work", None), steve_core::Flushed::Held);
            assert!(!routed.is_busy("work"), "an unbound attempt can be retired");
            assert!(matches!(
                routed.release("work").unwrap(),
                steve_core::Released::Gone
            ));
            assert_eq!(routed.running_turns(), 0);
            assert_eq!(routed.agent_pid("work"), None);
            assert!(
                !cfg.data_dir.exists(),
                "the Hub must not prepare a workspace"
            );
        }
    }
}
