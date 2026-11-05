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
/// ended, cross-workspace or unrecognized permission settings fail closed. YOLO decisions remain
/// the coordinator's job; this does not silently promote Ask to FullAccess.
pub fn binding_for(store: &Store, session: &str) -> Option<Binding> {
    let s = store.sessions().get(session).ok()??;
    if s.status == "ended" || s.readonly != 0 || s.device_id != "hub" || s.source != "Steve" {
        return None;
    }
    let item = store.items().get(s.item_id.as_deref()?).ok()??;
    if item.workspace_id != s.workspace_id {
        return None;
    }
    let bot = match s.bot_id.as_deref() {
        Some(id) => Some(store.bots().get(id).ok()??),
        None => None,
    };
    if bot
        .as_ref()
        .is_some_and(|b| b.workspace_id != s.workspace_id || !b.enabled)
    {
        return None;
    }
    let permissions = permission_mode(
        bot.as_ref()
            .map(|b| b.perm_mode.as_str())
            .unwrap_or("default"),
    )?;
    let cwd = if !s.cwd.is_empty() {
        Some(s.cwd.as_str())
    } else {
        item.cwd.as_deref().filter(|p| !p.is_empty())
    };
    let prepare = if let Some(cwd) = cwd {
        // Never replace a legacy workspace containing user changes with a fresh worktree.
        let path = Path::new(cwd);
        let dir = if path.file_name().and_then(|n| n.to_str()) == Some(item.id.as_str())
            && (item.project_id.is_none() || item.branch.is_some())
        {
            RootPath {
                root: root_id(path.parent()?)?,
                rel: RelPath::new(item.id.clone()).ok()?,
            }
        } else {
            at(cwd)?
        };
        PrepareKind::Existing { dir }
    } else if let Some(pid) = &item.project_id {
        let p = store.projects().get(pid).ok()??;
        if p.workspace_id != s.workspace_id || p.device_id != "hub" || p.archived {
            return None;
        }
        if p.kind == "git" && p.worktree {
            PrepareKind::Worktree {
                repository: at(&p.path)?,
                branch: format!("steve/{}", item.id.to_lowercase()),
                base: None,
            }
        } else {
            PrepareKind::Existing { dir: at(&p.path)? }
        }
    } else {
        PrepareKind::Scratch
    };
    Some(Binding {
        session: s.id,
        resource: format!("item:{}", item.id),
        prepare,
        choice: HarnessChoice {
            harness: s.harness,
            model: s.model,
            effort: s.reasoning_effort,
        },
        policy: ExecutionPolicy {
            revision: Revision::new(format!("host-{}", bot.as_ref().map_or(0, |b| b.revision)))
                .ok()?,
            permissions,
            sandbox: SandboxRequirement::None,
            readable: vec![],
            network: NetworkScope::Unrestricted,
            tools: GrantId::new("hub").ok()?,
        },
    })
}

/// The binding of a new session to the paired remote Node `node` (`STEVE_NODE_BIND`), or `None`
/// when it must stay where it would go otherwise. A remote Node has none of the Hub's directories,
/// so only an item without a project qualifies, in a fresh scratch workspace on that Node, and only
/// while every earlier session of the item ran there too (its files are there, not on the Hub).
/// The Node must be paired and active (a paused or revoked Node takes no new work), and paired by
/// an owner of the session's workspace: any signed-in user can pair a Node, and the Node sees the
/// session's prompts, tool output and files, so a member's own machine never gets others' work.
/// `speaks_auto`: the Node is known to speak [`AUTO_MODE`]; a session in `auto` goes to it only
/// then (an older Node would refuse to open it, for good), and otherwise stays where it would go.
pub fn remote_binding_for(
    store: &Store,
    session: &str,
    node: &NodeId,
    speaks_auto: bool,
) -> Option<Binding> {
    let mut b = binding_for(store, session)?;
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
            let state = steve_store::nodes::get(c, node.as_str())?.map(|n| n.state);
            // the user who paired the Node owns the session's workspace
            let owned: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes n JOIN workspace_members m ON m.user_id=n.owner
                  WHERE n.node_id=?1 AND m.workspace_id=?2 AND m.role='owner')",
                rusqlite::params![node.as_str(), s.workspace_id],
                |r| r.get(0),
            )?;
            // earlier sessions of the item that are not on this Node (on the Hub, or elsewhere)
            let elsewhere: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions x LEFT JOIN runtime_sessions r ON r.session_id=x.id
                  WHERE x.item_id=?1 AND x.id<>?2 AND (r.node_id IS NULL OR r.node_id<>?3))",
                rusqlite::params![item.id, session, node.as_str()],
                |r| r.get(0),
            )?;
            Ok::<_, StoreError>(state.as_deref() == Some("active") && owned && !elsewhere)
        })
        .ok()?;
    if !ok {
        return None;
    }
    b.prepare = PrepareKind::Scratch;
    Some(b)
}

/// Boot-time allowlist: explicit configured roots plus project and local session directories. No
/// filesystem traversal and never `/`. Newly added project paths need a host restart before binding.
pub(crate) fn roots(
    store: &Store,
    cfg: &crate::config::Config,
) -> Result<Vec<steve_node::Root>, StoreError> {
    let paths = store.reader(|c| {
        let mut q = c.prepare("SELECT path FROM projects WHERE archived=0 AND device_id='hub' UNION SELECT cwd FROM sessions WHERE status<>'ended' AND readonly=0 AND source='Steve' AND device_id='hub' UNION SELECT cwd FROM items WHERE cwd IS NOT NULL")?;
        let paths = q.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok::<_, StoreError>(paths)
    })?;
    let mut paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    paths.extend([cfg.scratch.clone(), cfg.worktrees.clone()]);
    paths.sort();
    paths.dedup();
    Ok(paths
        .into_iter()
        .filter(|p| p != Path::new("/"))
        .filter_map(|path| {
            Some(steve_node::Root {
                id: root_id(&path)?,
                display: "local workspace".into(),
                path,
            })
        })
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
            binding_for(&h.store, "s1").unwrap().prepare,
            PrepareKind::Scratch
        ));
        let project = h.dir.path().join("project");
        let cwd = h.dir.path().join("worktrees/i1");
        h.store.transaction::<_,StoreError>(|tx|{
            tx.execute("INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','project',?,'git',1,1,1)",[project.to_str().unwrap()])?;
            tx.execute("UPDATE items SET project_id='p1' WHERE id='i1'",[])?;Ok(())
        }).unwrap();
        assert!(
            matches!(binding_for(&h.store,"s1").unwrap().prepare,PrepareKind::Worktree {branch,..} if branch=="steve/i1")
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
        let b = binding_for(&h.store, "s1").unwrap();
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
        assert!(binding_for(&h.store, "s1").is_none());
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
                 INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1),('default','m','member',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_a','u','a',zeroblob(32),'fa','active',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_p','u','p',zeroblob(32),'fp','paused',1);
                 INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_m','m','m',zeroblob(32),'fm','active',1);",
                c = cwd.display()
            ),
        );
        let a = NodeId::new("n_a").unwrap();
        // the Hub's directory is not the Node's: a fresh scratch workspace there
        let b = remote_binding_for(&store, "s1", &a, true).unwrap();
        assert!(matches!(b.prepare, PrepareKind::Scratch));
        assert!(matches!(
            binding_for(&store, "s1").unwrap().prepare,
            PrepareKind::Existing { .. }
        ));
        // a paused or unknown Node takes no new work
        assert!(remote_binding_for(&store, "s1", &NodeId::new("n_p").unwrap(), true).is_none());
        assert!(remote_binding_for(&store, "s1", &NodeId::new("n_none").unwrap(), true).is_none());
        // a Node a plain member paired never gets the workspace's sessions
        assert!(remote_binding_for(&store, "s1", &NodeId::new("n_m").unwrap(), true).is_none());
        // an earlier session of the item on the Hub keeps the item's work there
        exec(
            &store,
            "INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s0','default','i1','claude-code','','ended',0,0);",
        );
        assert!(remote_binding_for(&store, "s1", &a, true).is_none());
        // unless it ran on that very Node
        exec(
            &store,
            "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES ('s0','n_a','{}','s0','r','released',0);",
        );
        assert!(remote_binding_for(&store, "s1", &a, true).is_some());
        // a project's directories are on the Hub
        exec(
            &store,
            "INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','p','/tmp/p','git',1,1,1);
             UPDATE items SET project_id='p1' WHERE id='i1';",
        );
        assert!(remote_binding_for(&store, "s1", &a, true).is_none());
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
                     INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES ('i1','default',1,'t','run','执行','dev',1,1);
                     INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','dev','claude-code','','active',1,1);",
                )?;
                Ok(())
            })
            .unwrap();
        let b = binding_for(&store, "s1").expect("the Steve 开发 bot binds");
        assert_eq!(b.policy.permissions, PermissionMode::Auto);
        assert_eq!(b.policy.revision.as_str(), "host-3");
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
        assert!(remote_binding_for(&store, "s1", &a, false).is_none());
        // other modes are unaffected
        assert!(remote_binding_for(&store, "s2", &a, false).is_some());
        let b = remote_binding_for(&store, "s1", &a, true).expect("a Node that speaks auto");
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
    async fn default_off_and_real_local_host_with_fake_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::config::Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(dir.path().join("steve").to_string_lossy().into()),
            "STEVE_REPO" => Some(dir.path().join("unused").to_string_lossy().into()),
            _ => None,
        })
        .unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let off: Vars = Arc::new(|_| None);
        let mut jobs = vec![];
        assert!(
            super::super::node_runtime(&cfg, &store, Arc::new(Ends), &off, &mut jobs)
                .unwrap()
                .is_none()
        );
        assert!(jobs.is_empty());
        assert!(!cfg.data_dir.join("node").exists());
        let on: Vars = Arc::new(|k| (k == "STEVE_NODE_MODE").then(|| "local".into()));
        cfg.no_resume = true;
        assert!(super::super::node_runtime(&cfg, &store, Arc::new(Ends), &on, &mut jobs).is_err());
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
        } = super::super::node_runtime(&cfg, &store, Arc::new(Ends), &on, &mut jobs)
            .unwrap()
            .unwrap();
        tools.install(Arc::new(NoTools)).unwrap();
        for job in jobs {
            job.await;
        }
        let b = binding_for(&store, "s1").unwrap();
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
}
