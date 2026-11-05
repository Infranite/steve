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

/// Steve's permission mode for a harness or bot mode name. `None` (including `auto`): the Node path
/// cannot express it, fail closed.
pub(crate) fn permission_mode(perm: &str) -> Option<PermissionMode> {
    Some(match perm {
        "plan" => PermissionMode::Plan,
        "default" | "ask" => PermissionMode::Ask,
        "acceptEdits" | "accept_edits" => PermissionMode::AcceptEdits,
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
        let on: Vars = Arc::new(|k| (k == "STEVE_NODE").then(|| "local".into()));
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
    fn permission_modes_map_and_auto_fails_closed() {
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
        assert_eq!(permission_mode("auto"), None);
        assert_eq!(permission_mode(""), None);
    }
}
