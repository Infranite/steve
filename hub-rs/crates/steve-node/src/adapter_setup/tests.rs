use super::*;
use crate::{LocalNode, NodeConfig, PolicyCeiling, Storage};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    dir: tempfile::TempDir,
    home: PathBuf,
    config: NodeConfig,
}

fn write_executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn command(id: &str) -> Command<InstallHarness> {
    Command {
        id: OperationId::new(id).unwrap(),
        issued_at: Timestamp(crate::now()),
        accept_before: Timestamp(crate::now() + 60_000),
        grant: GrantId::new("owner").unwrap(),
        body: InstallHarness {
            harness: "pi".into(),
        },
    }
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("steve");
        let node = dir.path().join("node");
        let real = executable(Path::new("node")).expect("test Node.js");
        write_executable(
            &node,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo v26.0.0; else exec '{}' \"$@\"; fi\n",
                real.display()
            ),
        );
        // The fake is only a regression fixture. Actual published Pi/MCP is exercised separately
        // through an isolated running Hub; a fixture handshake is not real Pi acceptance.
        let adapter = dir.path().join("fixture.js");
        let marker = serde_json::to_string(&dir.path().join("provider")).unwrap();
        let mcp = serde_json::to_string(&dir.path().join("no-mcp")).unwrap();
        std::fs::write(&adapter, format!("const provider={marker}, noMcp={mcp};\n") + r#"
import readline from 'node:readline';
import {existsSync} from 'node:fs';
readline.createInterface({input:process.stdin}).on('line', line => {
  const m=JSON.parse(line), send=(r)=>console.log(JSON.stringify({jsonrpc:'2.0',id:m.id,...r}));
  if (m.method==='initialize') send({result:{protocolVersion:1,agentCapabilities:{loadSession:true,mcpCapabilities:{http:!existsSync(noMcp)}}}});
  else if (m.method==='session/new' || m.method==='session/load') {
    if (!existsSync(provider)) send({error:{code:-32000,message:'Configure an API key or log in with an OAuth provider.'}});
    else send({result:{sessionId:'fixture-session',modes:{availableModes:[{id:'off'},{id:'high'}]},models:{currentModelId:'fixture/test',availableModels:[{modelId:'fixture/test',name:'Fixture'}]},configOptions:[{id:'thought_level',category:'thought_level',currentValue:'off',options:[{value:'off'},{value:'high'}]}]}});
  } else if (m.method==='session/prompt') send({result:{stopReason:'end_turn'}});
  else if (m.method==='session/set_mode') send({error:{code:-32602,message:'thinking is not a permission'}});
  else send({result:{}});
});
"#).unwrap();
        write_executable(
            &dir.path().join("npm"),
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{base}/npm.log'
if [ "$1" = view ]; then echo '"1.5.2"'; exit 0; fi
touch '{base}/started'
while [ -f '{base}/hold' ]; do sleep 0.02; done
if [ -f '{base}/fail' ]; then echo 'fixture-secret-authenticated-registry' >&2; exit 17; fi
p="$3/node_modules"
mkdir -p "$p/.bin" "$p/@earendil-works/pi-coding-agent/dist/bundle" "$p/@openma/pi-acp/dist"
echo '{{"version":"1.5.2","type":"module"}}' > "$p/@openma/pi-acp/package.json"
echo '{{"version":"1.5.2"}}' > "$p/@earendil-works/pi-coding-agent/package.json"
cp '{base}/fixture.js' "$p/@openma/pi-acp/dist/bin.js"
echo '// bootstrap fixture' > "$p/@openma/pi-acp/dist/mcp-bootstrap.js"
echo '// SDK entry' > "$p/@earendil-works/pi-coding-agent/dist/bundle/cli.js"
echo '#!/bin/sh' > "$p/.bin/pi"
chmod +x "$p/.bin/pi"
"#,
                base = dir.path().display()
            ),
        );
        let config = NodeConfig {
            node: NodeId::new("local").unwrap(),
            home: home.join("node"),
            acp: AcpConfig {
                adapters: HashMap::from([("pi".into(), managed::command(&home, "pi").unwrap())]),
                declared: HashMap::new(),
                node,
                claude_executable: "unused".into(),
                steve_home: home.clone(),
                codex_home: dir.path().join("codex"),
                user_home: dir.path().to_path_buf(),
                version: "test".into(),
            },
            roots: vec![],
            capacity: Capacity {
                sessions: 4,
                jobs: 4,
                execs: 4,
            },
            ceiling: PolicyCeiling::unlimited(),
            storage: Storage::default(),
        };
        Self { dir, home, config }
    }

    fn mark(&self, name: &str) {
        std::fs::write(self.dir.path().join(name), "").unwrap();
    }
    fn unmark(&self, name: &str) {
        std::fs::remove_file(self.dir.path().join(name)).unwrap();
    }
    fn node(&self) -> LocalNode {
        let c = &self.config;
        LocalNode::open(NodeConfig {
            node: c.node.clone(),
            home: c.home.clone(),
            acp: c.acp.clone(),
            roots: c.roots.clone(),
            capacity: c.capacity.clone(),
            ceiling: c.ceiling.clone(),
            storage: c.storage.clone(),
        })
        .unwrap()
    }
    fn add_component(&self, which: &str) {
        let c = adapters::components("pi")
            .iter()
            .find(|c| c.id == which)
            .unwrap();
        let root = managed::root(&self.home);
        let file = root.join(c.entry);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "// present").unwrap();
        let package = root.join(c.package);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("package.json"), r#"{"version":"1.5.2"}"#).unwrap();
        for required in c.required {
            let file = package.join(required);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "// present").unwrap();
        }
    }
}

async fn wait_finished(n: &LocalNode, op: &OperationId) -> Receipt {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let r = n.operation(op.clone()).await.unwrap();
            if r.state.is_finished() && n.0.adapter_maintenance.lock().unwrap().is_none() {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("installation finished")
}

#[test]
fn component_states_distinguish_empty_and_each_partial_install() {
    for installed in [None, Some("pi"), Some("adapter")] {
        let t = Fixture::new();
        if let Some(c) = installed {
            t.add_component(c);
        }
        let n = t.node();
        let req = Requirements {
            npm: Some(t.dir.path().join("npm")),
            node_error: None,
            npm_error: None,
        };
        let s = setup(&n.0, "pi", None, &req).unwrap().unwrap();
        assert_eq!(s.state, SetupState::NotInstalled);
        assert_eq!(s.components.len(), 2);
        assert!(s.can_install);
        for c in s.components {
            assert_eq!(c.installed, installed == Some(c.id.as_str()));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn install_is_async_deduplicated_and_immediately_rediscovered_without_restart() {
    let t = Fixture::new();
    t.mark("hold");
    t.mark("provider");
    let n = t.node();
    assert_eq!(
        n.probe(false).await.unwrap().harnesses[0]
            .setup
            .as_ref()
            .unwrap()
            .state,
        SetupState::NotInstalled
    );
    let c = command("install_one");
    let first = n.install_harness(c.clone()).await.unwrap();
    assert!(!first.state.is_finished());
    assert_eq!(
        n.install_harness(c.clone()).await.unwrap().operation,
        first.operation
    );
    let mut conflict = c.clone();
    conflict.body.harness = "codex".into();
    assert_eq!(
        n.install_harness(conflict).await.unwrap_err().code,
        Code::Conflict
    );
    assert_eq!(
        n.install_harness(command("install_duplicate"))
            .await
            .unwrap()
            .operation,
        first.operation
    );
    let caps = tokio::time::timeout(Duration::from_secs(2), n.probe(true))
        .await
        .expect("installing probe never waits for npm")
        .unwrap();
    assert_eq!(
        caps.harnesses[0].setup.as_ref().unwrap().state,
        SetupState::Installing
    );
    assert!(!caps.harnesses[0].setup.as_ref().unwrap().can_install);
    let blocked = n.start_job(job_command()).await;
    assert_eq!(blocked.unwrap_err().code, Code::Busy);
    assert!(
        Lease::reserve(&n.0, "other-adapter").is_none(),
        "auto update shares admission"
    );
    t.unmark("hold");
    assert!(matches!(
        wait_finished(&n, &c.id).await.state,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    let caps = n.probe(false).await.unwrap();
    let h = &caps.harnesses[0];
    assert_eq!(h.setup.as_ref().unwrap().state, SetupState::Ready);
    assert!(
        h.setup
            .as_ref()
            .unwrap()
            .components
            .iter()
            .all(|c| c.installed)
    );
    assert_eq!(
        h.adapter.as_ref().unwrap().session.as_ref().unwrap().modes,
        [PermissionMode::FullAccess]
    );
    assert_eq!(
        h.adapter.as_ref().unwrap().session.as_ref().unwrap().models[0].id,
        "fixture/test"
    );
    assert_eq!(
        n.install_harness(command("retry_after_success"))
            .await
            .unwrap()
            .operation,
        c.id,
        "a retry after the successful response was lost cannot run npm again"
    );
    let args = std::fs::read_to_string(t.dir.path().join("npm.log")).unwrap();
    assert_eq!(
        args.lines().filter(|l| l.starts_with("install ")).count(),
        1
    );
    assert!(
        args.contains("@earendil-works/pi-coding-agent@latest")
            && args.contains("@openma/pi-acp@latest")
            && args.contains("--global=false")
    );
    assert!(
        managed::selection(&t.home)
            .unwrap()
            .iter()
            .any(|i| i == "pi")
    );
    n.shutdown().await;
}

fn job_command() -> Command<JobSpec> {
    // Reuse the existing valid execution-policy fixture; admission rejects this job before it
    // could run during installation.
    let mut c = steve_node_api::testkit::build::cmd(
        "job_blocked",
        steve_node_api::testkit::build::job("job", "attempt", "fixture"),
    );
    c.issued_at = Timestamp(crate::now());
    c.accept_before = Timestamp(crate::now() + 60_000);
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn failure_is_safe_to_show_and_retry_then_provider_setup_is_still_required() {
    let t = Fixture::new();
    t.mark("fail");
    let n = t.node();
    let c = command("install_fail");
    n.install_harness(c.clone()).await.unwrap();
    assert!(matches!(
        wait_finished(&n, &c.id).await.state,
        Observed::Finished {
            outcome: Outcome::Failed,
            ..
        }
    ));
    let h = n.probe(false).await.unwrap().harnesses.remove(0);
    let s = h.setup.unwrap();
    assert_eq!(s.state, SetupState::Unavailable);
    assert!(s.can_install);
    assert!(!s.error.unwrap().contains("fixture-secret"));
    t.unmark("fail");
    let c = command("install_retry");
    n.install_harness(c.clone()).await.unwrap();
    wait_finished(&n, &c.id).await;
    let h = n.probe(false).await.unwrap().harnesses.remove(0);
    assert_eq!(h.setup.unwrap().state, SetupState::Unavailable);
    assert!(
        h.adapter
            .unwrap()
            .session_error
            .unwrap()
            .contains("Configure an API key")
    );
    t.mark("provider");
    assert_eq!(
        n.probe(true).await.unwrap().harnesses[0]
            .setup
            .as_ref()
            .unwrap()
            .state,
        SetupState::Ready
    );
    t.mark("no-mcp");
    let h = n.probe(true).await.unwrap().harnesses.remove(0);
    assert_eq!(h.setup.unwrap().state, SetupState::Unavailable);
    let a = h.adapter.unwrap();
    assert!(a.error.or(a.session_error).unwrap().contains("HTTP MCP"));
    n.shutdown().await;
}

#[tokio::test]
async fn missing_or_old_node_refuses_install_and_missing_npm_is_actionable() {
    let mut t = Fixture::new();
    t.config.acp.node = t.dir.path().join("missing-node");
    let n = t.node();
    assert_eq!(
        n.probe(true).await.unwrap().harnesses[0]
            .setup
            .as_ref()
            .unwrap()
            .state,
        SetupState::Unavailable
    );
    let e = n
        .install_harness(command("install_missing_node"))
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert!(e.safe_detail.contains("Node.js"));
    let r = Requirements {
        npm: None,
        node_error: None,
        npm_error: Some("npm is missing".into()),
    };
    let s = setup(&n.0, "pi", None, &r).unwrap().unwrap();
    assert!(!s.can_install && s.reason.unwrap().contains("npm is missing"));
    assert!(!t.dir.path().join("npm.log").exists());
    n.shutdown().await;
    let t = Fixture::new();
    write_executable(&t.config.acp.node, "#!/bin/sh\necho v20.0.0\n");
    let n = t.node();
    assert!(
        n.install_harness(command("install_old_node"))
            .await
            .unwrap_err()
            .safe_detail
            .contains("22.19.0")
    );
    n.shutdown().await;
}

#[tokio::test]
async fn custom_commands_and_remote_owner_ceiling_are_never_overwritten() {
    let mut t = Fixture::new();
    t.config
        .acp
        .adapters
        .insert("pi".into(), t.dir.path().join("custom.js"));
    let n = t.node();
    assert_eq!(
        n.install_harness(command("install_override"))
            .await
            .unwrap_err()
            .code,
        Code::Unsupported
    );
    assert!(
        !n.probe(false).await.unwrap().harnesses[0]
            .setup
            .as_ref()
            .unwrap()
            .can_install
    );
    n.shutdown().await;
    let mut t = Fixture::new();
    t.config
        .acp
        .declared
        .insert("pi".into(), steve_acp::Declaration::default());
    let n = t.node();
    assert_eq!(
        n.install_harness(command("install_declared"))
            .await
            .unwrap_err()
            .code,
        Code::Unsupported
    );
    n.shutdown().await;
    let mut t = Fixture::new();
    t.config.ceiling = PolicyCeiling::default();
    let n = t.node();
    assert_eq!(
        n.install_harness(command("install_denied"))
            .await
            .unwrap_err()
            .code,
        Code::Forbidden
    );
    n.shutdown().await;
}

#[tokio::test]
async fn interrupted_installation_is_unknown_until_an_explicit_retry() {
    let t = Fixture::new();
    let n = t.node();
    let c = command("interrupted");
    n.accept("install_harness", &c, |tx| {
        tx.set_installation("pi", &c.id)?;
        Ok(crate::Decided::accepted())
    })
    .unwrap();
    *n.0.adapter_maintenance.lock().unwrap() = None;
    n.0.j.tx(|t| t.mark_unknown()).unwrap();
    let caps = n.probe(false).await.unwrap();
    let s = caps.harnesses[0].setup.as_ref().unwrap();
    assert_eq!(s.state, SetupState::Unknown);
    assert!(s.can_install);
    assert!(
        !t.dir.path().join("npm.log").exists(),
        "probe never retries an interrupted install"
    );
    let retry = command("retry_unknown");
    n.install_harness(retry.clone()).await.unwrap();
    wait_finished(&n, &retry.id).await;
    n.shutdown().await;
}

#[tokio::test]
async fn active_execution_and_upgrade_exclude_manual_installation() {
    let t = Fixture::new();
    let n = t.node();
    let active = job_command();
    n.accept("start_job", &active, |_| Ok(crate::Decided::accepted()))
        .unwrap();
    assert_eq!(
        n.install_harness(command("install_while_active"))
            .await
            .unwrap_err()
            .code,
        Code::Busy
    );
    assert!(Lease::reserve(&n.0, "pi").is_none());
    n.0.j
        .tx(|t| {
            t.set_state(
                &active.id,
                &Observed::Finished {
                    outcome: Outcome::Succeeded,
                    result: None,
                    error: None,
                },
            )
        })
        .unwrap();
    let lease = Lease::reserve(&n.0, "pi").unwrap();
    assert_eq!(
        n.install_harness(command("install_while_upgrading"))
            .await
            .unwrap_err()
            .code,
        Code::Busy
    );
    drop(lease);
    assert!(!t.dir.path().join("npm.log").exists());
    n.shutdown().await;
}

#[tokio::test]
async fn the_prefix_lease_excludes_other_nodes_jobs_and_cli_installers() {
    let t = Fixture::new();
    let n = t.node();
    let running = managed::prefix_lock(&t.home, false).unwrap();
    assert_eq!(
        n.install_harness(command("another_node_running"))
            .await
            .unwrap_err()
            .code,
        Code::Busy
    );
    assert!(Lease::reserve(&n.0, "pi").is_none());
    assert!(
        managed::install(&t.dir.path().join("npm"), &t.home, &managed::packages("pi")).is_err()
    );
    assert!(!t.dir.path().join("npm.log").exists());
    drop(running);
    let c = command("other_node_released");
    n.install_harness(c.clone()).await.unwrap();
    wait_finished(&n, &c.id).await;
    n.shutdown().await;
}

#[tokio::test]
async fn thinking_levels_cannot_widen_permission_modes() {
    let t = Fixture::new();
    let n = t.node();
    for mode in [
        PermissionMode::Plan,
        PermissionMode::Ask,
        PermissionMode::AcceptEdits,
        PermissionMode::Auto,
    ] {
        let mut job = job_command();
        job.body.run.choice.harness = "pi".into();
        job.body.run.policy.permissions = mode;
        assert_eq!(n.start_job(job).await.unwrap_err().code, Code::Unsupported);
        assert!(
            n.0.cfg
                .acp
                .mode_choice("pi", mode)
                .resolve(Some(&["off".into(), "high".into()]))
                .is_err()
        );
        let mut research =
            steve_node_api::testkit::build::research("research", "research_attempt", "fixture");
        research.run.choice.harness = "pi".into();
        research.run.policy.permissions = mode;
        let mut c = job_command();
        c.body = research;
        assert_eq!(
            n.start_job(c).await.unwrap_err().code,
            Code::Unsupported,
            "research must refuse unsupported permissions before probing a provider"
        );
    }
    assert_eq!(
        n.0.cfg
            .acp
            .mode_choice("pi", PermissionMode::FullAccess)
            .resolve(Some(&["off".into(), "high".into()]))
            .unwrap(),
        None
    );
    n.shutdown().await;
}

#[tokio::test]
async fn a_recorded_orphan_installer_blocks_new_installs_and_execution() {
    use std::os::unix::process::CommandExt;
    let t = Fixture::new();
    let n = t.node();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = i64::from(child.id());
    let started = crate::procs::start_time(pid);
    n.0.j
        .tx(|t| {
            t.put_proc(
                &process_key(),
                pid,
                0,
                n.incarnation(),
                "installer-fixture",
                started,
            )
        })
        .unwrap();
    assert_eq!(
        n.install_harness(command("orphan_install"))
            .await
            .unwrap_err()
            .code,
        Code::Busy
    );
    assert_eq!(
        n.start_job(job_command()).await.unwrap_err().code,
        Code::Busy
    );
    child.kill().unwrap();
    child.wait().unwrap();
    let c = command("after_orphan_gone");
    n.install_harness(c.clone()).await.unwrap();
    wait_finished(&n, &c.id).await;
    n.shutdown().await;
}

#[tokio::test]
async fn external_executables_keep_session_evidence_without_installer_node_or_metadata() {
    for id in adapters::builtin_ids() {
        let mut t = Fixture::new();
        t.mark("provider");
        let wrapper = t.dir.path().join("external-adapter");
        let node = executable(Path::new("node")).unwrap();
        write_executable(
            &wrapper,
            &format!(
                "#!/bin/sh\nexec '{}' '{}'\n",
                node.display(),
                t.dir.path().join("fixture.js").display()
            ),
        );
        t.config.acp.adapters = HashMap::from([(id.to_string(), wrapper)]);
        t.config.acp.node = t.dir.path().join("missing-node");
        let n = t.node();
        let h = n.probe(true).await.unwrap().harnesses.remove(0);
        assert!(h.adapter.as_ref().unwrap().session.is_some(), "{id}: {h:?}");
        let Some(setup) = h.setup else {
            assert!(
                adapters::components(id).is_empty(),
                "{id}: npm setup evidence required"
            );
            n.shutdown().await;
            continue;
        };
        assert_eq!(setup.state, SetupState::Ready, "{id}");
        assert!(!setup.can_install);
        let adapter = setup.components.iter().find(|c| c.id == "adapter").unwrap();
        assert_eq!(adapter.status, Some(ComponentStatus::Unknown));
        assert!(
            !adapter.installed,
            "unknown integrity must not become installed=true"
        );
        let mut c = command("external_install");
        c.body.harness = id.into();
        assert_eq!(
            n.install_harness(c).await.unwrap_err().code,
            Code::Unsupported
        );
        n.shutdown().await;
    }
}

#[tokio::test]
async fn an_external_js_override_still_needs_its_actual_interpreter() {
    let mut t = Fixture::new();
    t.mark("provider");
    t.config
        .acp
        .adapters
        .insert("pi".into(), t.dir.path().join("fixture.js"));
    t.config.acp.node = t.dir.path().join("missing-node");
    let n = t.node();
    let h = n.probe(true).await.unwrap().harnesses.remove(0);
    assert!(h.adapter.unwrap().session.is_none());
    assert_eq!(h.setup.unwrap().state, SetupState::Unavailable);
    n.shutdown().await;
}
